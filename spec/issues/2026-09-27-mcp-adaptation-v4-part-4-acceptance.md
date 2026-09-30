# MCP 适配 v4-part-4 验收记录（wave 3：Workspace MCP 实例）

> 当前代码导航（2026-09-29）：Workspace handler、filesystem/Bash tools、descriptions 与行为测试现位于 `peri-mcp-workspace`；当前入口和有测试命中的验证命令见 [MCP packages 代码索引](../../docs/code-index/mcp-packages.md)。宿主 workspace context、client bridge 与 recovery lifecycle 测试仍在 `peri-middlewares/src/mcp/`。本记录内旧源码路径和命令保留为 wave 3 验收时点证据，不作为当前路径清单。**另：`peri-agent` / `peri-middlewares` 的 `error_suggest` 框架已整体删除（2026-09-29），本记录中相关 suggester 测试命令与覆盖清单不再可执行。**

## §0 元信息与证据纪律

| 项 | 值 |
| --- | --- |
| 波次 | W3（wave 3），范围见 `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-plan.md` §1 |
| 工作树 | `/Users/konghayao/code/ai/peri-v4p3`，分支 `feat/mcp-adaptation-v4-part-3` |
| 波次起点 HEAD | `f61f06e4`（wave 2 收口） |
| W3-A 提交 | `efe01549`（6 文件 / +764 −19：注册表 + handler 骨架 + AW3-11 seam 裁决） |
| 本记录对应的施工态（记录初版快照） | `efe01549` + 工作树未提交改动（`git status --porcelain` 共 97 项：90 修改 / 2 删除 / 5 新增 —— 第 97 项即本记录自身） |
| 树指纹（记录初版快照） | `git diff HEAD \| shasum -a 256` = `32903b47a2175a8e2b39daf3e71d5cc8561fe947a857d1913856f98748541d7d`（记录初版，**不再代表当前树**） |
| 补验批次 | 2026-09-27 夜：U1–U11 遗留任务补验（结果回填至 §1、§3.8、§4、§5.3、§7、§8、§9；**U5、U9 的运行时补验随后完成并已回填 §7**，含 §6 判定准则与 §8 行 10 同步） |
| 树指纹重取（补验前时点） | `git status --porcelain` = **12 项**；`git diff HEAD \| shasum -a 256` = `e5527dd0d8752442e4fec4a785b4f77349e493b732d747c742ed07cf7d005edb`（采集于 2026-09-27 21:36:58，**在 U9/U5 补验与本次回填之前**；口径：本次回填编辑会使未提交计数 +1，故该指纹是补验前时点值） |
| 树指纹重取（提交前时点） | `git status --porcelain` = **14 项**（0 未跟踪：文档/指引 9 + 代码与测试 4 + 本记录 1）；`git diff HEAD \| shasum -a 256` = `15eda76ef92d2e0c4616ec4ee74c3eb0d6ccd64c358053338e9ed2fcaff906ee`（采集于 2026-09-27 22:23:35，**在 U5/U9 补验与全部回填之后、提交之前**；本行写入会使指纹再变，故记录的是写入前时点值） |
| 收尾批次与交付终态 | 交付批次 `5a005f2b..1547b840` 共 **9 提交**（2026-09-27 23:26:49–23:31:09），其上另有文档回填提交 `8bf05533`；交付 HEAD `1547b840`、工作树干净（**采集时点 23:33**：`git status --porcelain` 空、`git diff HEAD` 指纹 `e3b0c442…`；23:48 起并行 campaign 在本工作树的未提交改动见 §3.10 ③ 补记）；终态门禁、生产二进制验收、既有 flake 定性、破坏必红与证据更正登记见 **§3.10** |
| 唯一裁决源 | `spec/issues/2026-09-27-mcp-adaptation-v4-part-4-plan.md`（v1 冻结） |

**证据纪律（三态分离）**：本记录每个断言都标注它属于哪一态——「目标归属」（设计文档声明）≠「当前实现」（代码事实，带 `仓库相对路径:行号`）≠「运行时证据」（命令原文 + exit code + 计数 + 现场打印）。**运行时不成立的一律进 §7 UNVERIFIED 清单**，不得据实现文件宣称「已落地」（plan R8）。

**迁移后的模型面名字**：`mcp__workspace__Read` / `mcp__workspace__Write` / `mcp__workspace__Edit` / `mcp__workspace__Glob` / `mcp__workspace__Grep` / `mcp__workspace__folder_operations` / `mcp__workspace__Bash`（7 项）。

---

## §1 三态判定总表

| # | 判定面 | 目标归属（设计文档） | 当前实现（代码事实） | 运行时证据 | 判定 |
| --- | --- | --- | --- | --- | --- |
| 1 | 7 个本地工具改由 `workspace` builtin 实例供给 | `docs/design/mcp-adaptation-v4-part-1.md:52`（`system_mcp_tools: ["Read","Write","Edit","Glob","Grep"]` 形态）与 `:63`（「对 Agent 暴露时继续使用现有 MCP effective tool name，避免不同 MCP 的同名工具冲突」） | `peri-acp-types/src/builtin_mcp.rs:123-190`（`WORKSPACE_TOOLS`，7 项全 `direct: true`）+ `:222-227`（实例条目，`policy_key: "WorkspaceMiddleware"`）；`peri-middlewares/src/mcp/builtin/workspace.rs:123`（真实 `ServerHandler`，按 cwd 包装 7 个既有实现）；`peri-middlewares/src/mcp/builtin/dispatch.rs:148-151`（工厂 arm，**无条件**返回 `Some`） | print 路径 5 个请求全部 `total=17 mcp=10 workspace=7 bare=[]`（`/tmp/w3_smoke3/W3/reqs/req-1..5.json`）；TUI `/mcp` 面板 `workspace ✔ connected tools: 7`（`/tmp/w3_tui_smoke/w3tui/screens/07-mcp-panel.txt`） | ✅ |
| 2 | 模型面名字为 effective name；裸名不再是任何直供路径 | 同上 | 注册表冻结字面量（`effective_name`）；声明段唯一来源 `peri-middlewares/src/tool_search/declaration.rs:57-60`（`prompt_declaration().or_else(builtin_declaration)`） | req 表 `bare=[]`（5/5 请求）；`--bare` 负对照 `mcp=0 workspace=0 bare=[]`（`/tmp/w3_neg/bare/reqs/req-1.json` 只有 7 个非 MCP 工具） | ✅ |
| 3 | 声明段文本与工具面一致（7 项模板逐字保留） | AW3-03 / S6 | `peri-acp-types/src/builtin_mcp.rs` 的 7 条 `prompt_declaration` 与实现侧逐字一致（A1 的字节级比对，7/7） | 首个请求 system 文本实测：``Run a shell command → `mcp__workspace__Bash` (Mcp Workspace Bash)`` | ✅ |
| 4 | 关闭键迁移（`FilesystemMiddleware`/`TerminalMiddleware` → `WorkspaceMiddleware`） | AW3-06 | `peri-acp-types/src/meta_harness.rs`：`BUILTIN_INSTANCE_POLICY_KEYS` 含 `WorkspaceMiddleware`；`MIDDLEWARE_NAMES` 已无前两键；`MIDDLEWARE_TOOL_NAMES` 已无 7 裸名 | `cargo test -p peri-acp-types --lib`（§3.1） | ✅ |
| 5 | session 级 seam（`TaskManager` + `on_bg_complete`） | plan §2 AW3-11 与 §2.1 | 接收端 `peri-middlewares/src/mcp/builtin/context.rs:84-98`（`WorkspaceInstanceInput`）、`:121`（`BuiltinInstanceContext.workspace`）、`:158`（`with_workspace`）；re-export `peri-middlewares/src/assembly.rs:27`；宿主注入 `peri-acp/src/host/assemble.rs:141/294/392-393`（早于 `run_initialize` 的 spawn）；产生点 `peri-acp/src/host/workspace.rs:91-111` | 4 个新用例（§3.5），含 `Arc::ptr_eq` 同一性与 inbox 投递 | ✅ |
| 6 | capability root（文件工具越界访问 / Bash 无沙箱） | AW3-04（明示本波不引入） | **不引入**：`peri-middlewares/src/mcp/builtin/workspace.rs:11-16` 逐条登记为缺口 | **越界基线已取回（补验 U3）**：`Read` 读 `/etc/hosts`、相对上跳 `../../../../../../etc/hosts`、`Write` 写出 cwd、`Bash` 越界执行**四条全部成功**；证据指针 `/tmp/u238_lingering/U3/run.stamped`（exit=0，wall 7s；旁证 `ls -la /tmp/u238_lingering/outside_write.txt`） | ⚠️ 未落地（**越界基线已取回**，根因登记为 wave 4（W4-1）；见 §7 U3） |
| 7 | 归一闭合 N1–N10 | AW3-07 | 见 §6 | 见 §3.5 | ✅ |

**总判定**：范围 §1.1 的 7 工具迁移、注册表/关闭键迁移、session 级 seam、归一闭合均已落地并有运行时证据；AW3-04 的 capability root 按裁决**不属本波交付**，如实记为缺口。

---

## §2 交付物清单

`git status --porcelain` 共 97 项，按顶层目录归类（第 97 项 `?? spec/issues/2026-09-27-mcp-adaptation-v4-part-4-acceptance.md` 为本记录自身）：

| 目录 | 项数 | 说明 |
| --- | --- | --- |
| `peri-middlewares` | 45 | 主角：`mcp/builtin/{workspace,dispatch,context,mod}.rs` 与各自测试、`assembly.rs`/`assembly/preparation.rs`/`assembly/workflow.rs`（摘除链槽）、`middleware/{mod,filesystem,terminal}.rs`、`subagent/{fork,mod}.rs`、`error_suggest/**`（5 suggester）、`permission/mod.rs`、`tool_search/declaration_test.rs`、`CLAUDE.md` |
| `peri-acp` | 18 | `host/{assemble,workspace,mod,stdio/mod}.rs`（seam 发送端）、`host/requests/session_lifecycle.rs`（三路径）、`session/{mod,construction}.rs`、`session/command/rewind.rs`（N9）、`prompt/prompt_test.rs`、`provider/{config,store}_test.rs`、`CLAUDE.md` |
| `peri-acp-types` | 1 | `src/meta_harness.rs`（关闭键） |
| `peri-agent` | 9 | `session/factory.rs`（链槽）、`session/exec/stage_builder/{tools,tools_test,builder_v2_test}.rs`、`agent/compact_v2/full.rs`（N10）、`session/mod.rs` |
| `peri-tui` | 9 | `kit/tool_display.rs`、`kit/acp_types/{tool_card,current_turn}.rs`、`kit/message_area/render/tool_card.rs`（N4–N7）+ 测试；`launch.rs`/`cli_print.rs`（`workspace_input: None`） |
| `docs` | 10 | `code-index/{peri-middlewares,peri-acp-types,peri-agent,peri-acp,peri-tui}.md`、`reference/mcp-ecosystem.md`、`meta-harness.md`、`design/{middleware-system,meta-harness}.md`、`standards/architecture-contracts.md` |
| `example` | 2 | `minimal/.peri/settings.json`、`minimal/README.md`（关闭键示例） |
| `scripts` | 1 | `import-exemptions.conf`（`host/workspace.rs` 的 `assembly` 边界豁免登记） |
| `spec` | 1 | 本波主计划（AW3-08/N9/N10/C5 修订） |

**结构性新增/删除**（5 新增 + 2 删除；5 个未跟踪文件中第 5 个是本记录自身）：

| 状态 | 路径 | 作用 |
| --- | --- | --- |
| 新增 | `peri-middlewares/src/mcp/builtin/workspace.rs` | `WorkspaceMcpServer`（W3-A 已提交，W3-B 增补 seam 转交） |
| 新增 | `peri-acp/src/host/workspace_seam_test.rs` | 宿主侧 seam 证据（`Arc::ptr_eq` 与 inbox 投递） |
| 新增 | `peri-agent/src/session/bg_complete.rs` + `bg_complete_test.rs` | session 级 `OnBgCompleteFn` 工厂（lazy resolve inbox） |
| 新增 | `peri-middlewares/tests/workspace_tool_registration.rs` | 跨 crate 注册面回归 |
| 删除 | `peri-middlewares/src/middleware/filesystem.rs` | 链槽摘除的必然结果（工具面已迁入实例） |
| 删除 | `peri-middlewares/tests/middleware_tool_registration.rs` | 同上（其断言面已由 `workspace_tool_registration.rs` 接管） |


---

## §3 命令台账

> 台账只记录**可复核的原文**：命令、exit code、`test result:` 行、命中名单、现场打印。日志留档路径逐条给出。三态中，本节全部属于「运行时证据」态。

### §3.1 D1 全量门禁（终态复跑，2026-09-27 19:43–19:45，日志 `/tmp/d3_gate2/`）

| 段 | 命令原文 | exit | 关键输出（逐字） |
| --- | --- | --- | --- |
| 构建 | `cargo build --workspace` | 0 | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 0.27s`；仅一条预存在的链接器提示 `ld: __eh_frame section too large (max 16MB)…`（非本次改动引入） |
| lib 测试 | `cargo test --workspace --lib` | 0 | 16 个 target 全绿：**6609 passed / 0 failed / 15 ignored**（逐 target 见下表） |
| doc 测试 | `cargo test --workspace --doc` | 0 | `11 passed; 0 failed; 5 ignored` |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished \`dev\` profile … in 0.40s`（无 warning） |
| fmt | `cargo fmt --all --check` | 0 | 无输出 |
| 层门 | `bash scripts/check-layer-imports.sh` | 0 | `§0 依赖门：共 18 条规则，违规边 0` |
| rustdoc | `cargo doc --workspace --no-deps` | 0 | `Finished … in 0.28s` |

**16 target 逐项计数**（`/tmp/d3_gate2/lib.log`，按 `Running unittests src/lib.rs (target/debug/deps/<crate>-<hash>)` 顺序）：

| # | target | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| 1 | `langfuse_client` | 97 | 0 | 0 |
| 2 | `peri_acp` | 738 | 0 | 0 |
| 3 | `peri_acp_types` | 456 | 0 | 0 |
| 4 | `peri_agent` | 883 | 0 | 0 |
| 5 | `peri_controller` | 131 | 0 | 0 |
| 6 | `peri_js_runtime` | 50 | 0 | 0 |
| 7 | `peri_lsp` | 98 | 0 | 0 |
| 8 | `peri_middlewares` | 1986 | 0 | 5 |
| 9 | `peri_model` | 156 | 0 | 0 |
| 10 | `peri_process` | 2 | 0 | 0 |
| 11 | `peri_resources` | 156 | 0 | 0 |
| 12 | `peri_runtime` | 16 | 0 | 0 |
| 13 | `peri_theme` | 24 | 0 | 0 |
| 14 | `peri_tui` | 1684 | 0 | 7 |
| 15 | `peri_web_pty` | 24 | 0 | 0 |
| 16 | `peri_workflow` | 108 | 0 | 3 |
| — | **合计** | **6609** | **0** | **15** |

**时间异常的先验说明（自我披露）**：复跑时 build/lib/clippy 的耗时近乎为零，是 cargo 内容指纹命中（`Finished … in 0.27s`）；同样四段在 19:33–19:35 的首轮（`/tmp/d3_gate/`，构建为完整耗时）给出同一计数与同一 exit 0，可互为旁证。**新鲜度校验**：`find <workspace> -newermt '2026-09-27 19:45:50' -name '*.rs'` 无输出 ⇒ 门禁日志晚于所有源文件改动。

### §3.2 D2 print 路径正向 smoke（`/tmp/w3_smoke3/W3/`，17:58）

运行方式：`SCENARIO_FILE=…/scenario.json REQ_DIR=…/reqs node /tmp/w3_smoke3/fake-model.js`（假 Anthropic Messages API SSE 服务，脚本化多轮）+ `target/debug/peri -p … --output-format stream-json`。

脚本编排 4 个工具调用：`mcp__workspace__Write`（写 `work/src/main.rs`）→ `mcp__workspace__Read`（读回）→ `mcp__workspace__Bash`（`echo w3-bash-marker`）→ `SearchExtraTools`（query=`cron`）。

**① 首个请求工具表**（5 个携带工具表的请求 `req-1..5.json`，逐字核对）：

```
req-1.json total=17 mcp=10 ws=7 bare=[]
req-2.json total=17 mcp=10 ws=7 bare=[]
req-3.json total=17 mcp=10 ws=7 bare=[]
req-4.json total=17 mcp=10 ws=7 bare=[]
req-5.json total=17 mcp=10 ws=7 bare=[]
```

（`req-6.json` 的 `tools=[]`——该请求是「预测输入」辅助调用 `<prediction_directive>`，非工具表请求，一并列出以免误读。）

**② 真实执行输出**（`run.stdout` 逐字）：

```
{"type":"tool_result","id":"t1","output":"Wrote 1 line src/main.rs"}
{"type":"tool_result","id":"t2","output":"     1\tfn main() { println!(\"w3-write-marker\"); }\n     2\t"}
{"type":"tool_result","id":"t3","output":"w3-bash-marker\n"}
```

⇒ `mcp__workspace__Write` / `Read` / `Bash` 三者均经实例真实执行并返回可核对文本。

**③ 搜索面**：`t4`（query=`cron`）返回 `{"results":[{"name":"mcp__cron__cron_remove",…},{"name":"mcp__cron__cron_register",…},{"name":"mcp__cron__cron_list",…},{"name":"goal",…},{"name":"DynamicMCP",…}],"total_available":10}` —— 延迟面按 effective name 正常返回；逐工具 XOR 见 §3.4。

**④ 搜索面 CSV 条目级 XOR 校验**（`node /tmp/w3_xor.js /tmp/w3_smoke3/W3/reqs/req-1.json`）：

```
=== /tmp/w3_smoke3/W3/reqs/req-1.json  (tools=17) ===
  工具表：effective 命中 7/7  裸名当工具名 []
  搜索面 CSV 条目数=17（条目级判定，非子串）
    Read              mcp__workspace__Read              eff=true bare=false OK
    Write             mcp__workspace__Write             eff=true bare=false OK
    Edit              mcp__workspace__Edit              eff=true bare=false OK
    Glob              mcp__workspace__Glob              eff=true bare=false OK
    Grep              mcp__workspace__Grep              eff=true bare=false OK
    folder_operations mcp__workspace__folder_operations eff=true bare=false OK
    Bash              mcp__workspace__Bash              eff=true bare=false OK

GLOBAL_XOR: PASS
```

（口径说明：XOR 判定为**条目级**——CSV 中「恰有 `mcp__workspace__X` 这一项、恰无 `X` 这一项」，不是子串搜索；理由：`mcp_read_resource` 等名字含子串 `Read` 却不构成裸名条目，子串口径会误报。）

### §3.3 D2 负对照（`/tmp/w3_neg/`）

命令：`--bare` 启动同一假模型链路（runner `/tmp/w3_neg/run.sh`，注释原文「D2 负对照（本人独立执行）：--bare 下无 MCP 池 ⇒ 不应出现任何 mcp__* 工具」）。

```
/tmp/w3_neg/bare/reqs/req-1.json total=7 mcp=0 ws=0 bare=0
```

⇒ 7 个 `mcp__workspace__*` 的出现确由 MCP 池/实例供给，不是别处的静态注入；`--bare` 下连裸名也不出现（工具面只剩 7 个非 MCP 工具）。

### §3.4 D2 搜索面/RPC 面逐工具 XOR 运行时探针（`/tmp/w3_xor/run/`，XOR 闸门专用）

场景：`x1` `SearchExtraTools(query="Read")` → `x2` `SearchExtraTools(query="Bash")` → `x3` `ExecuteExtraTool(tool_name="mcp__workspace__Read")` → `x4` `ExecuteExtraTool(tool_name="Read")`。

**搜索结果的条目名（逐条实测）**：

```
req-2.json x1 entry_names= ['mcp_read_resource', 'DiscoverMCP', 'AgentResult', 'DynamicMCP', 'mcp__cron__cron_register']
req-3.json x2 entry_names= ['RunPtcCode', 'AgentResult', 'Workflow', 'mcp_read_resource', 'mcp__cron__cron_remove']
```

⇒ 查询 `Read` **不返回**任何名为 `Read` 的条目（`mcp_read_resource` 是 MCP 资源工具，名字不同）；查询 `Bash` **不返回**任何名为 `Bash` 的条目 —— 搜索面负半 XOR 成立（direct 工具不入延迟索引）。

**执行面**：

```
req-4.json x3 → tool_result: "     1\tfn main() {}\n     2\t"     （effective name 经 ExecuteExtraTool 真实解析并读文件）
req-5.json x4 → tool_result: "Tool not found: Read"                （裸名恰不解析）
```

⇒ 正半 XOR（effective name 恰命中）+ 负半 XOR（裸名恰不命中）在 **RPC 面**同时成立。

**CSV 条目级校验**（`node /tmp/w3_xor.js /tmp/w3_xor/run/reqs/req-1.json`）输出与 §3.2 ④ 同形，`GLOBAL_XOR: PASS`。

### §3.5 D2 TUI 路径 smoke（`/tmp/w3_tui_smoke/w3tui/`，18:03）

同一假模型 + 假 LSP（`fake-lsp.js`）脚本化 TUI 会话。**迁移前基线对照**（同一 harness，10:44–10:45）：

| 目录 | 时点 | req-1 工具表 |
| --- | --- | --- |
| `/tmp/w3_tui_smoke/tui/` | 迁移前 | `total=17 ws=0 bare=7` |
| `/tmp/w3_tui_smoke/tui-hitl/` | 迁移前（HITL 场景） | `total=17 ws=0 bare=7` |
| `/tmp/w3_tui_smoke/tui1/` | 迁移前 | `total=17 ws=0 bare=7` |
| `/tmp/w3_tui_smoke/w3tui/` | **迁移后** | `total=17 ws=7 bare=0` |

**④ `/mcp` 面板**（`screens/07-mcp-panel.txt` 逐字）：

```
MCP Pool:ready5/5 connected

 > artifact  ✔ connected
transport: builtin  tools: 1  skills: 0
   cron  ✔ connected
transport: builtin  tools: 3  skills: 0
   lsp  ✔ connected
transport: builtin  tools: 0  skills: 0
   web  ✔ connected
transport: builtin  tools: 2  skills: 0
   workspace  ✔ connected
transport: builtin  tools: 7  skills: 0
```

⇒ 面板显示 `workspace` 已连接、`transport: builtin`、`tools: 7`；轮次正常收尾（`03-after-turn.txt` 含 `TUI_SMOKE_DONE`）。

### §3.6 定向证据批次（终态复跑，19:47，日志 `/tmp/d3_ev2/`）

runner 原文（每条命令记录 `### CMD:` 横幅 + `### EXIT:` + 命中名单），18 条**全部** `EXIT: 0`：

| # | 命令原文（`### CMD:` 横幅） | `test result:` |
| --- | --- | --- |
| 01 | `cargo test -p peri-acp-types --lib -- builtin_mcp::tests` | `ok. 11 passed; 0 failed; 0 ignored; 445 filtered out` |
| 02 | `cargo test -p peri-acp-types --lib -- meta_harness::tests` | `ok. 8 passed; 0 failed; 0 ignored; 448 filtered out` |
| 03 | `cargo test -p peri-middlewares --lib -- mcp::builtin::` | `ok. 86 passed; 0 failed; 0 ignored; 1905 filtered out` |
| 04 | `cargo test -p peri-middlewares --lib -- subagent::fork::tests` | `ok. 26 passed; 0 failed; 0 ignored; 1965 filtered out` |
| 05 | `cargo test -p peri-middlewares --lib -- subagent::tests` | `ok. 34 passed; 0 failed; 0 ignored; 1957 filtered out` |
| 06 | `cargo test -p peri-middlewares --lib -- error_suggest::` | `ok. 39 passed; 0 failed; 0 ignored; 1952 filtered out` |
| 07 | `cargo test -p peri-middlewares --lib -- permission::tests` | `ok. 34 passed; 0 failed; 0 ignored; 1957 filtered out` |
| 08 | `cargo test -p peri-middlewares --lib -- assembly::tests` | `ok. 38 passed; 0 failed; 0 ignored; 1953 filtered out` |
| 09 | `cargo test -p peri-middlewares --lib -- tool_search::` | `ok. 64 passed; 0 failed; 0 ignored; 1927 filtered out` |
| 10 | `cargo test -p peri-middlewares --test workspace_tool_registration` | `ok. 1 passed; 0 failed; 0 ignored; 0 filtered out` |
| 11 | `cargo test -p peri-tui --lib -- kit::tool_display::tests` | `ok. 16 passed; 0 failed; 0 ignored; 1675 filtered out` |
| 12 | `cargo test -p peri-tui --lib -- kit::acp_types::tests` | `ok. 37 passed; 0 failed; 0 ignored; 1654 filtered out` |
| 13 | `cargo test -p peri-tui --lib -- kit::message_area::render` | `ok. 103 passed; 0 failed; 0 ignored; 1588 filtered out` |
| 14 | `cargo test -p peri-acp --lib -- session::command::rewind` | `ok. 36 passed; 0 failed; 0 ignored; 702 filtered out` |
| 15 | `cargo test -p peri-acp --lib -- host::workspace_seam` | `ok. 2 passed; 0 failed; 0 ignored; 736 filtered out` |
| 16 | `cargo test -p peri-agent --lib -- agent::compact_v2` | `ok. 169 passed; 0 failed; 0 ignored; 714 filtered out` |
| 17 | `cargo test -p peri-agent --lib -- session::bg_complete` | `ok. 4 passed; 0 failed; 0 ignored; 879 filtered out` |
| 18 | `cargo test -p peri-agent --lib -- session::exec::stage_builder` | `ok. 6 passed; 0 failed; 0 ignored; 877 filtered out` |

**关键具名用例（逐条从日志抄录，非 `0 tests`）**：

- 注册表/实例：`builtin_mcp::tests::{instance_identity_equals_server_name, wave1_tools_are_all_declared_direct, original_tool_name_of_effective_hits_frozen_literals}`；`meta_harness::tests::{builtin_instance_policy_keys_match_declaration_table, known_key_union_covers_builtin_policy_keys, middleware_names_and_builtin_policy_keys_are_disjoint}`
- handler 线路级（`mcp::builtin::workspace::tests` 7 例）：`workspace_handler_handshakes_and_lists_seven_tools_over_wire`、`workspace_handler_write_then_read_round_trips_in_host_cwd_over_wire`、`workspace_handler_bash_runs_in_the_same_host_cwd_over_wire`、`workspace_handler_unknown_tool_is_invalid_params_over_wire`、`workspace_handler_tool_failures_return_sanitized_error_result_over_wire`、`workspace_handler_bash_run_in_background_uses_injected_task_manager_over_wire`（正向）、`workspace_handler_without_session_input_rejects_run_in_background_over_wire`（`None` 退化）
- seam：`host::workspace_seam::{session_environment_holds_the_workspace_input_it_injected, session_bg_complete_callback_defers_into_the_registered_session}`；`session::bg_complete::tests::{test_inbox_is_resolved_lazily_at_call_time, test_shell_completion_is_delivered_as_defer_with_shell_source, test_missing_inbox_is_silent_noop, test_callback_result_is_assignable_to_acp_types_alias}`
- 归一闭合：`subagent::fork::tests::{explorer_disallow_matches_workspace_effective_name, coder_allowlist_matches_workspace_effective_name}`；`subagent::tests::{fully_disallowed_with_workspace_effective_names_is_readonly, capability_whitelist_effective_name_disallowed_by_bare_name_is_readonly}`；`error_suggest::*_test::test_*_matches_workspace_effective_name`（5 个 suggester）与 5 条 `test_*_skips_unregistered_mcp_instance` 反例；`permission::tests::{workspace_sensitive_entries_use_effective_names, sensitive_entries_use_effective_names_and_parity_description, builtin_effective_names_match_original_name_policy, wave1_frozen_effective_name_policy, cron_sensitive_markdown_uses_registry_effective_name}`
- TUI：`kit::tool_display::tests::workspace_effective_names_map_to_short_names`（+ 既有 `tui_has_no_hardcoded_effective_name`、`tui_unknown_mcp_names_fall_back_to_generic`）；`kit::acp_types::tests::{workspace_edit_builds_diff, has_running_bash_tool_matches_workspace_effective_name}`；`kit::message_area::render::tests::{workspace_bash_card_keeps_dollar_prefix, workspace_read_and_edit_cards_keep_header_suffix}`
- N9/N10：`session::command::rewind::tests::{rewind_collects_workspace_effective_write_and_edit, rewind_ignores_unknown_effective_names}`；`agent::compact_v2::full::tests::{extract_recent_files_matches_workspace_effective_read, extract_recent_files_ignores_unknown_effective_names, extract_skills_paths_matches_workspace_effective_read, extract_skills_paths_ignores_unknown_effective_names}`
- 链摘除后的行为断言：`assembly::tests::{middleware_names_match_production_blueprint, middleware_tool_names_match_static_tool_sets, workflow_shell_tool_face_is_the_workspace_bridge, meta_harness_disables_each_known_middleware}`；`session::exec::stage_builder::tools::tests::migrated_naked_names_are_no_longer_excluded`
- 跨 crate 注册面：`workspace_tool_registration`（`peri-middlewares/tests/`，公开 API 外部验收）

### §3.7 提交前终态复核（2026-09-27 20:26–20:29，随提交同批）

| 门 | 命令原文 | exit | 结果（逐字） |
| --- | --- | --- | --- |
| lib 全量 | `cargo test --workspace --lib --no-fail-fast`（日志 `/tmp/w3_final_ws/ws_lib.log`） | 0 | 16 个 target 全部 `test result: ok.`；合计 **6609 passed / 0 failed / 15 ignored** —— 与 §3.1 的数字逐字一致（跨两次独立运行的复现性交叉验证） |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings`（日志 `/tmp/w3_final_clippy/clippy.log`） | 0 | `warning`/`error` 行计数 **0**；`Finished … in 0.45s` 表明 cargo 指纹命中 ⇒ 该结果对应的正是当前源码状态（无重编译） |
| fmt | `cargo fmt --all --check` | 0 | 无输出 |
| 层门 | `bash scripts/check-layer-imports.sh` | 0 | 18 条规则 / 违规边 **0** |

**时间窗校验（证明「终态」不是自述）**：本轮启动 20:26:39；工作树内受跟踪源码的最新 mtime 为 **19:18:16**（`peri-acp/src/host/executor_flow_test.rs`）⇒ 结果覆盖当前终态；此后唯一写入是**本记录自身**（未跟踪文件，不参与编译）。

### §3.8 遗留补验批次（U6/U7 新增用例，2026-09-27 夜，日志 `/tmp/u67_lingering/`）

**新增 6 例**（本轮 U6/U7 补验的施工产物）：

| # | 用例（`仓库相对路径:行号`） | 覆盖 |
| --- | --- | --- |
| 1 | `peri-middlewares/src/mcp/builtin/workspace_test.rs:582` `workspace_handler_foreground_timeout_kills_the_group_without_input_and_promotes_with_input_over_wire` | U6-1：`None`（无注入输入）⇒ 杀进程组；`Some`（注入 manager）⇒ 保留进程组（被提升） |
| 2 | `peri-middlewares/src/mcp/builtin/workspace_test.rs:687` `foreground_timeout_text_and_evidence_differ_without_and_with_task_manager` | U6-1：两形态的文本 / 状态 / `task_id` 三者互斥 |
| 3 | `peri-middlewares/src/mcp/builtin/workspace_test.rs:778` `foreground_timeout_with_rejected_promotion_falls_back_to_killing_the_group` | U6-1 前提修正：提升**被注册表拒绝**（`SHELL_LIMIT` 满）的回落分支 |
| 4 | `peri-middlewares/src/mcp/builtin/workspace_test.rs:859` `workspace_handler_lingering_child_is_registered_only_with_injected_task_manager_over_wire` | U6-2：`command &` 残留子进程的登记对照 |
| 5 | `peri-agent/src/session/bg_complete_test.rs:235` `test_cancelled_background_shell_skips_completion_callback_and_defer` | U7：取消路径跳过回调（计数 0 / 队列空 / 无 wake） |
| 6 | `peri-agent/src/session/bg_complete_test.rs:305` `test_natural_background_shell_completion_delivers_defer_through_the_manager` | U7：自然完成对照（恰 1 条 `Defer` / `ShellComplete`） |

**命令与数据**（每条 3 连跑，逐字计数相同；日志 `/tmp/u67_lingering/targeted_FINAL.log`）：

| 命令原文 | exit | `test result:`（逐字） |
| --- | --- | --- |
| `cargo test -p peri-middlewares --lib -- mcp::builtin::workspace::tests` | 0 | `ok. 11 passed; 0 failed; 0 ignored; 0 measured; 1984 filtered out`（3 连跑） |
| `cargo test -p peri-agent --lib -- session::bg_complete::tests` | 0 | `ok. 6 passed; 0 failed; 0 ignored; 0 measured; 879 filtered out`（3 连跑） |

**破坏/还原台账**：5 次，见 `/tmp/u67_lingering/break{1,2,3,4a,4b,5}_run.log`；生产文件终态 sha 与基线一致（`terminal.rs dc78796f…`、`shell.rs 31432329…`）。

**门禁（补验批次终态）**：`cargo clippy -p peri-middlewares -p peri-agent --all-targets -- -D warnings` / `cargo fmt -p peri-middlewares -p peri-agent --check` / `bash scripts/check-layer-imports.sh` —— 三者均 `EXIT: 0`（日志 `/tmp/u67_lingering/{gate_clippy,gate_fmt,gate_layers}.log` 与 `gates_final.log`；层门输出 `§0 依赖门：共 18 条规则，违规边 0`）。

### §3.9 补验批次提交前终态复核（2026-09-27 22:13–22:23）

| 门 | 命令原文 | exit | 结果（逐字） |
| --- | --- | --- | --- |
| lib 全量 | `cargo test --workspace --lib --no-fail-fast`（日志 `/tmp/w3_final_gate/gate2.log`） | 0 | 16 个 target 全部 `test result: ok.`；合计 **6616 passed / 0 failed / 15 ignored** |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 无 warning/error；`Finished … in 7.08s`（指纹命中，无重编译） |
| fmt | `cargo fmt --all --check` | 0 | 无输出 |
| 层门 | `bash scripts/check-layer-imports.sh`（日志 `/tmp/w3_final_gate/layer.log`） | 0 | `§0 依赖门：共 18 条规则，违规边 0` |
| 探针残留 | `grep -rn "BREAK-\|COUNTER-EXPERIMENT\|TEMPORARY PROBE\|U6 侦察" peri-{acp,middlewares,agent,tui,acp-types}/src` | — | **零命中**（U5 补验用例的 `[W3-U5 补验]` 现场打印为**正式证据发射器**，非残留） |

**与 §3.7 的差分**：`6616 − 6609 = +7`（U6/U7 新增 6 例 + U5 新增 1 例），ignored 15 不变 ⇒ 两批数字互相印证。

**一次 flake 的登记（同批首跑）**：`22:13` 的首跑（`cargo test --workspace --lib`，无 `--no-fail-fast`）在 `-p peri-tui` 中止，`FAILED. 1682 passed; 2 failed`（`kit::acp_bridge::tests::{test_bridge_reset_rehydrates_pending_compact_note_for_same_session, test_hitl_bridge_drops_unowned_events_before_all_side_effects}`）。复核：① 单测过滤 `kit::acp_bridge::tests` ⇒ `ok. 17 passed`；② 全量 `-p peri-tui --lib` ⇒ `ok. 1684 passed; 0 failed`；③ 全量 workspace 重跑（上表）⇒ `6616 passed / 0 failed`。三次运行之间**源码零改动**（仅测试执行），两例均为 `#[serial]` 全局态用例、非本波改动面（`peri-tui` 的 W3 改动在 `tool_display` / `tool_card` / `current_turn`），判为**并发负载诱发的 flake**，不作红灯登记。

**U5/U9 批次证据（本轮新增）**：U5 补验用例与可证伪实验见 §7 U5 行（`mcp_v4_wave2_test.rs:668`；`assemble.rs:668` 破坏 ⇒ 必红 `:688` ⇒ 还原 sha `97224fd0…`）；U9 三组终态重放见 §7 U9 行（`/tmp/u9_lingering/g{1,2,2x,3}_*.log`）。

### §3.10 交付终态：收尾批次 + 终态门禁 + 生产二进制验收（2026-09-27 23:26–23:45）

> 本节是**交付终态**的收口证据：9 个收尾提交落地后的 HEAD、终态门禁、用真实 `target/debug/peri` 跑的正向验收、以及一条可复现的既有测试隔离缺陷登记。§3.1–§3.9 的采集时点更早；凡与时点相关的判断，以本节采集时刻为准。

**① 交付身份**（采集 2026-09-27 23:33:35）

| 项 | 值 |
| --- | --- |
| HEAD | `1547b84068339e17bd90afd11e0972cbe48ae53a`（分支 `feat/mcp-adaptation-v4-part-3`；`git rev-list --left-right --count origin/main...HEAD` = `0 65`，未推送、无 upstream） |
| 工作树 | `git status --porcelain` **空**；`git diff HEAD \| shasum -a 256` = `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`（空输入哈希） |
| 收尾批次 | `5a005f2b..1547b840` 共 **9 提交**（23:26:49–23:31:09；23 文件 / +467 −75；**新增用例 7、删除用例 0**） |
| 生产二进制 | `target/debug/peri`，261153640 bytes，mtime `2026-09-27 23:31:55`（晚于最后一次源码提交 23:31:09） |
| 记录回填提交 | `8bf05533`（本 §3.10 的终态补录：③ 补记 / ⑤ 补记 / ⑥ 破坏必红 / ⑦ 证据更正登记 + part-3 §7 矛盾修正）；本行由其后的小提交补记，故「当前 HEAD」请以 `git log -1` 为准 |

**② 收尾批次清单（逐条）**

| # | 提交 | 主题 | 落点 | 修复前 → 修复后 | 新增用例 |
| --- | --- | --- | --- | --- | --- |
| 1 | `f84bfec3` | fix(agent): compact 目标 token 改用 u64 饱和减法 | `peri-agent/src/agent/compact_v2/planner.rs` + 测试 | `reserve`（三个 u32 字段加宽求和，上界 3×u32::MAX）先**截断回 u32** 再做饱和减法：`reserve = 2^32` 时截断为 0 ⇒ 目标值反等于整个上下文窗口 ⇒ 改为先把 `context_window` 拓宽到 u64 再减 | `test_context_pressure_reserve_above_u32_range_saturates_target_to_zero` |
| 2 | `28b44f7c` | fix(middleware): `before_tools_batch` 结果数量错配 fail closed | `peri-agent/src/middleware/{chain,trait}.rs`、`peri-middlewares/src/permission/mod.rs`、`docs/code-index/peri-agent.md`、`docs/design/middleware-system.md` | 数量偏少时尾部槽位保持初始 `Ok(call)`（**未经审批的调用直接进入执行**，fail-open）、偏多时整段错位一位 ⇒ 数量不符即把未拒绝槽位整体置 `MiddlewareError` 并 `break`；`permission` 批量超时由「多插一条汇总拒绝」改为逐条等长 | `test_before_tools_batch_short_result_fails_closed`、`test_before_tools_batch_long_result_preserves_rejection_and_stops_chain`、`test_dispatch_batch_cardinality_error_stops_before_tool_execution`、`test_process_batch_broker_timeout_returns_one_result_per_call` |
| 3 | `1756162e` | fix(acp-types): Dynamic MCP 摘要面移除 URL userinfo 与请求数据 | `peri-acp-types/src/dynamic_mcp.rs` + 测试 + code-index | canonical URL 直接 `expect` ⇒ 反序列化构造的非法 URL 让摘要投影 panic；`user:password@host` 原样带进审批/状态视图 ⇒ 新增 `safe_http_url_summary`（非法即 `[invalid URL]`；合法则清 username / password / query / fragment） | `safe_summary_handles_malformed_canonical_url_without_echoing_it`（另改写既有 `..._removes_url_credentials_and_request_data`） |
| 4 | `0d11bebf` | refactor(agent): 删除无引用的 `TurnConfig` 副本 | `peri-agent/src/session/exec/executor/context.rs` | `#[allow(dead_code)]` 的陈旧副本（全仓唯一实际使用指向 `executor.rs` 自己那份）⇒ 删除 | — |
| 5 | `c47a372a` | refactor(middlewares): `ask_user` 描述抽为静态常量 | `peri-middlewares/src/ask_user/mod.rs`、`src/tools/ask_user_tool.rs` | `description()` 每次调用 `.leak()` 生成永不回收的内存 ⇒ 抽 `ASK_USER_TOOL_DESCRIPTION`，文本逐字未变 | — |
| 6 | `d1aae671` | fix(controller): 补打身份失败时保留发射方 `message_id` | `peri-controller/src/controller.rs` + 测试 + code-index 行号锚点 | fallback envelope 只复制 turn/agent/delivery，丢 `source.message_id` ⇒ 未登记 session 的迟到事件丢失消息级身份 ⇒ 构造后回填 | `publish_event_preserves_source_message_id_when_session_is_unregistered` |
| 7 | `d1807a4d` | docs(tui): 修正 `peri-tui/CLAUDE.md` 的 Scope 依赖描述 | `peri-tui/CLAUDE.md` | 原称直接依赖 `peri-agent`（manifest 无此依赖、`src/` 零引用）⇒ 按事实改写 | — |
| 8 | `c032bafa` | docs: 补全 `problems.md` 任务路由并登记全仓清理台账 | `spec/global/problems.md`、`spec/issues/2026-09-27-repository-cleanup.md` | Workflow 行指向已删除的 `spec/archive-issues/workflow/` ⇒ 拆行改指现行索引；新增清理台账（WP-01…WP-18 互斥划分） | — |
| 9 | `1547b840` | docs(mcp): 修正 part-3 验收记录 R29 回填与 §7.3/§7.6 判定不一致 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-3-acceptance.md`（已压缩至 `spec/history/2026-09.md`，原文见 Git 历史） | 同一文件自相矛盾（回填声称已更新，§7.3/§7.6 仍写「属真实缺口」）⇒ 只改仍为旧判定的行 | — |

**9 个提交逐个过了 `lefthook` pre-commit（`check`/`clippy`/`layer-imports`/`typos`/`fmt`；纯文档提交按文件过滤只跑后两段），日志 `/tmp/v4p3_commit_verify/commit{1..9}.log`。**

**③ 终态门禁**（2026-09-27 23:33:51–23:38，日志 `/tmp/v4p3_final_gate/`，末行 `ALL_GATES_DONE`）

| 门 | 命令原文 | exit | 关键输出（逐字） |
| --- | --- | --- | --- |
| 起始 | `git rev-parse HEAD; git status --porcelain; git diff HEAD \| shasum -a 256` | 0 | `1547b840…`；porcelain 空；指纹 `e3b0c442…` |
| 构建 | `cargo build --workspace` | 0 | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 11.21s`；1 条 linker warning（`ld: __eh_frame section too large (max 16MB)…`，`peri-tui` bin，既有） |
| lib 全量 | `cargo test --workspace --lib --no-fail-fast` | **101** | 16 target；合计 **6623 例 / 6621 passed / 2 failed / 15 ignored**；唯一失败 target 为 `peri_tui`（`FAILED. 1682 passed; 2 failed; 7 ignored`）—— **见 ⑤，既有隔离缺陷**。**复核重跑**（23:46:24–23:48:26，`/tmp/v4p3_terminal_gate/A2c_full_lib.log`）⇒ **exit 0、`6623 passed / 0 failed / 15 ignored`**（`peri_tui` ⇒ `ok. 1684 passed; 0 failed; 7 ignored`）；两次运行唯一差异即 ⑤ 的两例，红/绿取决于竞态 |
| doc | `cargo test --workspace --doc` | 0 | 16 个 doc-test target；11 passed / 0 failed / 5 ignored |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished … in 12.44s`（有重编译）；全文无 `warning`/`error` 行 |
| fmt | `cargo fmt --all --check` | 0 | 无输出 |
| rustdoc | `cargo doc --workspace --no-deps` | 0 | 0 error；153 条 rustdoc lint（8 个 crate，既有；`cargo doc` 未加 `-D warnings`） |
| 层门 | `bash scripts/check-layer-imports.sh` | 0 | `§0 依赖门：共 18 条规则，违规边 0` |
| 契约 | `cargo test -p peri-middlewares --test canonical_tool_invocation_contract` | 0 | `ok. 5 passed; 0 failed` |
| 契约 | `cargo test -p peri-middlewares --test mcp_host_policy_contract` | 0 | `ok. 5 passed; 0 failed` |
| 定向 10–21 | 12 条过滤器（下表） | 0 | 全部**非零命中且 0 failed**（无过滤器零命中假通过） |

**定向过滤器逐条（`test result:` 逐字）**：

| 过滤器 | `test result:` |
| --- | --- |
| `agent::compact_v2` | `ok. 170 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out` |
| `middleware::chain` | `ok. 28 passed; 0 failed; 0 ignored; 0 measured; 861 filtered out` |
| `agent::stages::tool_dispatch` | `ok. 21 passed; 0 failed; 0 ignored; 0 measured; 868 filtered out` |
| `permission::tests` | `ok. 35 passed; 0 failed; 0 ignored; 0 measured; 1961 filtered out` |
| `ask_user` | `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1979 filtered out` |
| `controller` | `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 115 filtered out` |
| `dynamic_mcp` | `ok. 14 passed; 0 failed; 0 ignored; 0 measured; 443 filtered out` |
| `builtin_mcp::tests` | `ok. 11 passed; 0 failed; 0 ignored; 0 measured; 446 filtered out` |
| `meta_harness::tests` | `ok. 8 passed; 0 failed; 0 ignored; 0 measured; 449 filtered out` |
| `host::mcp_v4_wave2::` | `ok. 12 passed; 0 failed; 0 ignored; 0 measured; 727 filtered out` |
| `mcp::tool_bridge` | `ok. 17 passed; 0 failed; 0 ignored; 0 measured; 1979 filtered out` |
| `mcp::builtin::` | `ok. 90 passed; 0 failed; 0 ignored; 0 measured; 1906 filtered out` |

② 的 7 个新增用例**全部落在上述命中集内**（因此「通过」由所属过滤运行的 0 failed 承载）：`middleware::chain::tests::{test_before_tools_batch_short_result_fails_closed, test_before_tools_batch_long_result_preserves_rejection_and_stops_chain}`、`agent::stages::tool_dispatch::tests::test_dispatch_batch_cardinality_error_stops_before_tool_execution`、`permission::tests::test_process_batch_broker_timeout_returns_one_result_per_call`、`agent::compact_v2::planner::tests::test_context_pressure_reserve_above_u32_range_saturates_target_to_zero`、`controller::*::publish_event_preserves_source_message_id_when_session_is_unregistered`、`dynamic_mcp::*::safe_summary_handles_malformed_canonical_url_without_echoing_it`。

**编译回归复核（历史两处红，必须显式判死）**：

| 历史失败（接收时点实测） | 终态实测 |
| --- | --- |
| `E0277`：`peri-agent/src/agent/stages/tool_dispatch_test.rs` 的 `.unwrap_err()`（`DispatchOutcome` 未实现 `Debug`）⇒ **peri-agent lib test 目标整体不可编译，过滤器「零执行」**（日志 `/tmp/v4p3_dirty_verify/01_compact_v2.log`，exit 101） | **不复现**：`cargo test --workspace --lib` 中 `peri_agent` ⇒ `ok. 889 passed; 0 failed`（该 lib test 目标须整体编译通过才可能产出此计数）；`agent::stages::tool_dispatch` ⇒ `ok. 21 passed` |
| `E0599`：`peri-acp-types/src/dynamic_mcp.rs:274` 的 `parsed.set_query(None).is_err()`（`Url::set_query` 返回 `()`）⇒ workspace 不可编译（日志 `/tmp/v4p3_dirty_verify/F9_acp_types.log`，exit 101） | **不复现**：`peri_acp_types` ⇒ `ok. 457 passed; 0 failed`；`dynamic_mcp` ⇒ `ok. 14 passed`；该行已由 `1756162e` 改为不对 `set_query`/`set_fragment` 调 `is_err` |

**与 §3.9 的差分**：`6623 − 6616 = +7`，`ignored` 15 不变 —— 恰为 ② 新增的 7 个用例（planner 1 / chain 2 / tool_dispatch 1 / permission 1 / dynamic_mcp 1 / controller 1）；其余 15 个 target 的计数逐项复核一致。

**③ 补记：复核重跑（2026-09-27 23:46–23:50，日志 `/tmp/v4p3_terminal_gate/`）**

| 复核项 | 命令原文 | exit | 结果（逐字） |
| --- | --- | --- | --- |
| lib 全量重跑 | `cargo test --workspace --lib` | 0 | 16 target 全绿；**6623 passed / 0 failed / 15 ignored**（`A2c_full_lib.log`；`peri_tui` ⇒ `ok. 1684 passed; 0 failed; 7 ignored`） |
| doc | `cargo test --workspace --doc` | 0 | 与上表同构（`A3_doc_tests.log`） |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished \`dev\` profile … in 7.43s`（`A4_clippy.log`） |
| 定向 `ask_user` | `cargo test -p peri-middlewares --lib -- ask_user` | 0 | `ok. 17 passed; 0 failed; 0 ignored; 1979 filtered out` |
| 定向 `permission` | `cargo test -p peri-middlewares --lib -- permission` | 0 | `ok. 75 passed; 0 failed; 0 ignored; 1921 filtered out` |
| 定向 `dynamic_mcp` | `cargo test -p peri-acp-types --lib -- dynamic_mcp` | 0 | `ok. 14 passed; 0 failed; 0 ignored; 443 filtered out` |
| 定向 `controller` | `cargo test -p peri-controller --lib -- controller` | 0 | `ok. 17 passed; 0 failed; 0 ignored; 115 filtered out`（**⑦ 行 1 的替代证据**） |
| `peri-tui` 单包 | `cargo test -p peri-tui --lib` | 0 | `ok. 1684 passed; 0 failed; 7 ignored`（`D16_peri_tui_lib.log`） |

**时点口径（诚实披露）**：本组起始于 23:46；**23:48:31 起**同一工作树内的并行清理 campaign 开始写 `langfuse-client/**` 与 `Cargo.lock`（未提交），23:49–23:50 又有 `peri-web-pty/**` 与 spec/docs 写入。**纯净态证据仍是 23:33–23:38 的主门禁**（起始工作树为空、全部门禁在其后 5 分钟内完成）；本组为复现性交叉验证（其中 `A2c` 于 23:48:26 结束，早于首次源码突变 23:48:31，其 6623/0 对 `1547b840` 源码有效；`A3`/`A4` 的 `langfuse-client` 槽位可能含并行改动，其余 crate 未被触碰）。另：门禁首跑的 `A2` 于 23:41:46 被前台 15 s 上限触发进程组清理终止，**不计入任何结论**，`A2c` 为最终有效值（以 `Popen(start_new_session=True)` 脱附运行）。

**④ 生产二进制正向验收**（23:33:51–23:34:48，日志 `/tmp/v4p3_final_accept/`，`ALL_SMOKES_DONE`；全程未改仓库、未重建）

| 通道 | 命令 | exit | 关键观察（本人抽查复核过的项标 ✔） |
| --- | --- | --- | --- |
| print W3 | `bash /tmp/w3_smoke3/run.sh W3` | 0 | 5 个工具表请求全部 `total=17 mcp=10 ws=7 bare=[]` ✔；`mcp__workspace__Write` ⇒ `Wrote 1 line src/main.rs`、`Read` 回读含 `w3-write-marker`、`Bash` ⇒ `w3-bash-marker\n`；搜索面命中 `mcp__cron__cron_register@1.0722` |
| print `/mcp` | `run.sh MCP` | 0 | `run.stdout` 仅 412B/3 行，与归档基线 `prev/MCP_1758` **逐字节同构**；print + stream-json 不渲染面板（口径不符，非回归，见 A1） |
| XOR 探针 | `bash /tmp/w3_xor/run.sh` | 0 | `x3` 真实执行 `mcp__workspace__Read` ⇒ 返回文件内容；`x4` 裸名 ⇒ 模型请求体内 `Tool not found: Read`（与基线逐字一致） |
| 批量探针 | `run_batch.sh`（port 53531，`requests=4`） | 0 | 轮 1：3 调用 ⇒ 3 结果 `b1/b2/b3` 全 `is_error=false` 且按 `tool_use_id` 对齐 ✔；轮 2：2 ⇒ 2；`MiddlewareError` / `returned N results for M calls` / `ToolRejected` 各 **0** 次 |
| TUI bypass | `tui-run.sh w3tui2 bypass` | 0 | `MCP Pool:ready5/5 connected` ✔、`workspace ✔ connected  transport: builtin  tools: 7`；`06-expanded-result.txt` 含 `已注册定时任务 01a0e380-c9db-…`；残留三项检查全空 |
| TUI HITL | `HITL_PROBE=1 tui-run.sh w3tui2-hitl default` | 0 | `09-approval.txt` 有 `!  Approval` 卡与 `Enter: approve \| Esc: reject` ✔；`10-after-approve.txt` 有 `✓ Allowed once` ✔ 且续跑至 `TUI_SMOKE_DONE`；`已注册定时任务 01a0e380-fc69-…` 见 `05-focus-2` / `06-expanded-result` / `08-final` ✔ 与 `reqs/req-3.json` |
| 收尾 | `08_after.log` | — | HEAD 仍 `1547b840…`；`git status --porcelain` 空 |

**二进制 ↔ 源码对应性**（本人独立复核）：`find . -name '*.rs' -newermt '2026-09-27 23:31:55'`（二进制 mtime）**无输出**，全部受跟踪文件亦无输出 ⇒ 上述 smoke 覆盖的正是 ①② 的源码终态。本项在 ⑥ 的破坏实验**之前**采集。

**差异归类（均非本批回归）**：

| # | 观察 | 归类 |
| --- | --- | --- |
| A1 | print `/mcp` 无面板输出 | 夹具口径：面板只在 TUI 通道渲染；与归档基线逐字节一致 |
| A2 | 批量探针 `b2` 读到写前内容 | 并发语义的必然结果（阶段二 `futures::future::join_all` + 保序回写）；核心意图「逐条执行、结果不错位」成立，非结果错位 |
| A3 | `x4` 的 `Tool not found` 只在模型请求体、不在 stream-json 输出 | 既有行为（基线的 `req-5.json` 同样含该行） |
| A4 | `SearchExtraTools` 0.4 分并列组第 5 席运行间不同 | 工具索引 `RwLock<HashMap>` 迭代序 + 稳定排序：**同 HEAD 连跑两次即不同** ⇒ 与本次提交无关，属可复现性缺口 |
| A5 | 夹具日志尾部 `Terminated: 15 … fake-model.js` | 夹具自身 `kill` 触发的 job-control 提示，脚本随后 `exit=0` |
| A6 | TUI HITL 轮询记录 `HIT=card` 而非 `HIT=result` | 观测时点：结果行在批准后渲染，轮询已因 `TUI_SMOKE_DONE` 退出；结果本身在 `05/06/08` 与 `req-3.json` 中可见 |

**残留清理**：收尾 `tmux ls` = `no server running on /private/tmp/tmux-501/default`；`pgrep -fl` 对 peri 二进制、三套夹具、`fake-model.js` 全部为空（本人另以 `ps aux` 独立复核，无 peri 进程）。

**⑤ flake → 缺陷：口径修正登记**

§3.9 把 `peri-tui --lib` 两例失败记为「并发负载诱发的 flake」。终态门禁把同一对用例**稳定复现**并定位到根因，故在此升级为**可复现的既有测试隔离缺陷**（不阻断本次交付，登记 §9 W4-11）：

| 复现方式 | 结果 |
| --- | --- |
| `cargo test --workspace --lib --no-fail-fast`（默认并行；`02_lib` 1 次 + 复核 5 次，共 6 次） | **3/6 失败**，失败集合恒为同 2 例 |
| 同一测试二进制 `--test-threads=1` 全量串行（1 次） | `ok. 1684 passed; 0 failed; 7 ignored`（49.71s） |
| 单例 `--exact`（2 次） | `ok. 1 passed; 0 failed` ×2 |
| `kit::acp_bridge` 模块串行（3 次） | `ok. 17 passed; 0 failed` ×3 |

失败原文（`/tmp/v4p3_final_gate/02_lib.log`）：

```
thread 'kit::acp_bridge::tests::test_hitl_bridge_drops_unowned_events_before_all_side_effects' panicked at peri-tui/src/kit/acp_bridge_test.rs:762:5:
assertion failed: VIEW_MODELS.state().read().items.iter().any(|vm| matches!(vm, TuiRenderUnit::TuiAskUserBlock(block) if block.request_id.as_deref() == Some("\"h\"")))
thread 'kit::acp_bridge::tests::test_bridge_reset_rehydrates_pending_compact_note_for_same_session' panicked at peri-tui/src/kit/acp_bridge_test.rs:391:5:
assertion failed: !crate::kit::atoms::ACP_STATE.state().read().is_loading
```

**根因（静态核查）**：两例本身都带 `#[serial]`，但 `serial_test` 只序列化 `#[serial]` 用例；同一 `peri-tui --lib` 二进制内的 `peri-tui/src/acp_client/client_reverse_test.rs` 有 **12 个 `#[tokio::test]` 未加 `#[serial]`**（515/575/617/657/675/694/732/782/787/792/858/894 行），却在 setup/teardown 里写同一批**进程级全局**（`VIEW_MODELS` / `ACP_STATE` / `HITL_PENDING` / `INPUT_BUFFER` / `ACTIVE_SESSION_ID` / `POPUP_KIND` / `FOLD_OVERRIDES`）。

**归属（非本批引入）**：`git diff --name-only f84bfec3^ 1547b840 -- 'peri-tui/**'` 仅 `peri-tui/CLAUDE.md` ⇒ 本批未改 `peri-tui` 源码；同一对用例在**批次落地前**的 22:13 已以同样计数失败（§3.9 的 flake 登记行，`FAILED. 1682 passed; 2 failed`，同样两例）⇒ **既有缺陷**。

**复核重跑的对照**（23:46:24–23:48:26；本次重跑前后工作树内**无 `.rs` 改动**，仅 2 个文档文件为未提交态 ⇒ 编译/执行针对的即是 HEAD `1547b840` 的源码）：`cargo test --workspace --lib` **exit 0**，16 target 全绿，合计 **6623 passed / 0 failed / 15 ignored**（`peri_tui` ⇒ `ok. 1684 passed; 0 failed; 7 ignored`，日志 `/tmp/v4p3_terminal_gate/A2c_full_lib.log`）。即在这 6 次并行红绿对照之外又抽到全绿的一次（累计 7 次并行：3 红 4 绿）—— 「失败集合恒为同 2 例（3/6 红）+ 串行必绿 + 单例必绿」共同支持「进程级全局态隔离缺陷」的定性，而非本批代码回归。

**⑥ 破坏必红（无修复必红反例实验，隔离 worktree；2026-09-27 23:46–23:52）**

装置：`git worktree add --detach /tmp/v4p3_mut 1547b840`（HEAD 全程恒定，交付工作树零写入），`CARGO_TARGET_DIR=/tmp/v4p3_mut_target`（冷编译预热 137.4s）。运行器 `/tmp/v4p3_mutation/run_isolated.py`（在 worktree 之外，cargo 以 `cwd=/tmp/v4p3_mut` 执行）。每个 case 四段，任一段不合格即中止：**baseline**（修复态必绿）→ **mutate**（锚点命中数必须恰为 1）→ **mutated_test**（必须 exit 101 且出现 `test result: FAILED` 且**无编译错误**）→ **restore**（`git checkout --` 后 `git status --porcelain` 必须为空，复跑必须复绿）。原始记录 `/tmp/v4p3_mutation/{results.json,REPORT.md}`；逐 case 日志 `/tmp/v4p3_mutation/<case>/{baseline,mutate,test,restore}.log`。

| # | 变异（把修复还原为修复前形态） | 基线 | 变异后 `test result:` | 变红用例 | 断言实据 | 还原 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | planner：`(context_window as u64).saturating_sub(reserve)` ← 旧式 `context_window.saturating_sub(reserve as u32) as u64` | `ok. 1 passed`（888 filtered） | `FAILED. 0 passed; 1 failed; … 888 filtered out` | `test_context_pressure_reserve_above_u32_range_saturates_target_to_zero` | `left: 16000 / right: 0` | porcelain 空；复跑 ok |
| 2a | chain：删「长度校验 + 整体置 MiddlewareError + break」并把回写改回 `if let Some(..)`（fail-open / 错位） | `ok. 28 passed`（861 filtered） | `FAILED. 26 passed; 2 failed; … 861 filtered out` | `test_before_tools_batch_short_result_fails_closed`、`…_long_result_preserves_rejection_and_stops_chain` | 两条真 `assert!`（`chain_test.rs:483/572`） | porcelain 空；复跑 ok 28 |
| 2b | 同一变异的第二个观察面 | `ok. 21 passed`（868 filtered） | `FAILED. 20 passed; 1 failed; … 868 filtered out` | `test_dispatch_batch_cardinality_error_stops_before_tool_execution` | 不变量断言（`Ok(_) ⇒ panic!("批量规模错配必须让 dispatch 失败…")`） | porcelain 空；复跑 ok 21 |
| 3 | permission：超时分支改回 `push(汇总条) + extend(逐条)` | `ok. 1 passed`（1995 filtered） | `FAILED. 0 passed; 1 failed; … 1995 filtered out` | `test_process_batch_broker_timeout_returns_one_result_per_call` | `left: 3 / right: 2` | porcelain 空；复跑 ok |
| 4 | controller：删 `envelope.message_id = source.message_id.clone()` 回填 | `ok. 1 passed`（131 filtered） | `FAILED. 0 passed; 1 failed; … 131 filtered out` | `publish_event_preserves_source_message_id_when_session_is_unregistered` | `left: None / right: Some("message-1")` | porcelain 空；复跑 ok |
| 5 | acp-types：`safe_http_url_summary` 还原为 `url::Url::parse(..).expect(..)` 且不清 userinfo | `ok. 14 passed`（443 filtered） | `FAILED. 12 passed; 2 failed; … 443 filtered out` | `public_dynamic_action_summary_removes_url_credentials_and_request_data`、`safe_summary_handles_malformed_canonical_url_without_echoing_it` | 前者真 `assert!`（`:152`）；后者 **panic**（`expect("canonical Dynamic MCP URL is valid")` 遇非法 URL） | porcelain 空；复跑 ok 14 |

**总判**：**5/5 case 有效** —— 锚点命中数 1 / (1,1) / 1 / 1 / 1 全部唯一；5 次变异运行均无 `error[E…]`、无 `could not compile`，exit 101 全部由**测试失败**产生；还原后 porcelain 逐次为空、复跑逐次复绿。⇒ 本批 **7 个新增用例全部为「无修复必红」的可证伪证据**，另带 1 个改写过的既有用例（`public_dynamic_action_summary_removes_url_credentials_and_request_data`）同样变红。② 行 2b 与行 5 的第二条失败落在被测不变量/生产代码的 `panic!`/`expect` 处而非测试内 `assert!`，判定仍有效（由变异直接引起，且同批另有真断言失败作为对照）。

**隔离性与未污染证据**：5 个修复文件的 sha256 **三方 MATCH**（HEAD / 交付工作树 / 隔离 worktree），`git diff -- <5 文件>` 为空 ⇒ 破坏实验期间交付工作树内这 5 个文件从未被改动；工作树内的并行 campaign 改动只落在 `langfuse-client/**`、`peri-web-pty/**`、`Cargo.lock`、spec/docs，与本批 5 文件无交集。实验后 `git worktree list` 回到原 4 条、`git worktree prune -n -v` 无输出、`/tmp/v4p3_mut` 与 `/tmp/v4p3_mut_target` 均已删除。

**⑦ 证据更正登记（「提交信息 vs 落档日志」失配，2026-09-27 23:47 对抗性复核）**

对 `5a005f2b..1547b840` 的只读对抗复核（`/tmp/v4p3_batch_review/REPORT.md`）裁定：**代码经静态审查可交付；记录有 1 项 BLOCKER（证据维度）+ 3 项 MAJOR，逐条更正后签收**。提交信息属已发布历史，**不改写**；更正统一登记于此（口径同 plan R8：不见实测不给 ✅）。

| # | 提交 | 原声明 | 复核所见 | 更正 / 替代证据 |
| --- | --- | --- | --- | --- |
| 1 | `d1aae671` | `cargo test -p peri-controller --lib -- controller` 通过（「见记录」） | `/tmp/v4p3_commit_verify/04_peri-controller_lib_controller.log` 实为 **`E0599` 编译失败**（当时 `dynamic_mcp.rs:274` 的 `.is_err()` 尚未修）；`commit6.log` 只覆盖 check/clippy/fmt | 以 ③ 为准：`controller` ⇒ `ok. 17 passed; 0 failed; 0 ignored; 115 filtered out`（含新增用例）；另见 ⑥ 的破坏实验（该用例可证伪）；代码静态审查无缺陷，**不回滚** |
| 2 | `c47a372a` | `ask_user` ⇒ 17 passed | 无对应日志 | 以 ③ 为准：`ask_user` ⇒ `ok. 17 passed; 0 failed; 0 ignored; 1979 filtered out` |
| 3 | `f84bfec3` | `cargo fmt --all --check` 干净 | 无对应成功日志；同目录唯一相邻的 `commit1.log` 是一次 **fmt 失败中止**的提交尝试（无 commit 行） | 以 ③ 为准：终态 `cargo fmt --all --check` exit 0 无输出；`commit2.log` / `commit6.log` 的 lefthook `fmt` 段为工作区级 ✔ |
| 4 | `c47a372a` | 「解码后 **317** 字节一致」 | 实测两侧均 **316** 字节、逐字相同 | 数字以 316 为准（文本判定不变） |
| 5 | `28b44f7c` | 「去掉 break 时两条断言同时红」 | 按字面为假：`chain.rs:175` 的 `break` 与既有空守卫（`:151-153`）行为冗余，仅删 `break` 该用例仍绿 | 该用例实际证伪的是「长度校验缺失」；`break` 冗余登记为 **W4-13** |
| 6 | `0d11bebf` / `d1aae671` | 「post-commit 钩子」 | `lefthook.yml` 定义的是 **`pre-commit`** | 名称以 `pre-commit` 为准 |
| 7 | `c032bafa` | 「`spec/archive-issues/workflow/` 由 `2864a97d` 删除」 | 该提交对该子路径 **0 命中**；目录随 `a89258ee`（2026-09-09）消失 | 结论（路径已死）成立，归因更正 |
| 8 | — | — | `/tmp/v4p3_commit_verify/04_peri-middlewares_test*.log` 是 **cargo 参数错误**（无测试运行） | 不得作通过证据；middlewares 证据以 ③ 定向过滤器 + §3.7 契约测试为准 |
| 9 | `28b44f7c` 批量超时文案 | 「沿用既有超时文案」（属实） | 文案写死 `BROKER_TIMEOUT`(300s) 而非 `self.broker_timeout`（单条路径同病，既有） | 登记为 **W4-12** |
| 10 | `1547b840` | 「消除 part-3 记录的自相矛盾」 | 复核指出残留两处：part-3 `:363` §7 前言、`:152` 计数口径 | **本批已修**：`:363`/`:413` 前言改为「§7.3 已带 R29 运行时证据」的表述；`:152`「五例」改为「四例（连同本行）」，与该文件 `:437`/`:448` 一致 |

---

## §4 语义变更登记（S1–S7，逐条实测判定）

> 来源：plan §3.5。每条给出「判定 + 证据 + 残余 UNVERIFIED」。凡运行时未观察到的一律不给 ✅。

| # | 变更 | 实测判定 | 证据 |
| --- | --- | --- | --- |
| **S1** | 模型面工具名由裸名变 `mcp__workspace__<Name>`（7 项） | ✅ **已实测（6 个观测面中 5 个有运行时证据，1 个 UNVERIFIED）** | ① 直连表：print 5 个工具表请求 + TUI 均为 `ws=7 bare=[]`（§3.2 ①、§3.5）；② 搜索面：CSV 条目级 XOR PASS + 搜索/执行 RPC 探针（§3.4）；③ 审批面：首个请求 system 文本的敏感清单 14 条中 4 条已是 effective name（见下）；④ 事件载荷 / TUI 展示：`kit::acp_types::tests::{workspace_edit_builds_diff, has_running_bash_tool_matches_workspace_effective_name}`（§3.6 12）；⑤ PTC 目录：`RunPtcCode` 描述内的 `Tools directly available in this session:` CSV 含 7 项 effective name（§3.2 ④）；⑥ **Langfuse 观测名：UNVERIFIED**（未接 Langfuse，见 §7）。**补验（本轮）：见 §7 U1** —— 已闭合（tool observation 的 `name` 逐字 = effective name） |
| **S2** | `## Deferred Tools` 摘要面不再含这 7 个名字 | ✅ **已实测** | 首个请求 system 文本中 `## Deferred Tools` 段共 10011 字符，条目头为 `AgentResult / DiscoverMCP / DynamicMCP / RunPtcCode / Workflow / goal / mcp_read_resource`；对 7 个裸名逐条 `^- <name>\b` 正则匹配**全部 False**（§3.2 现场脚本） |
| **S3** | 关闭键从 `FilesystemMiddleware` / `TerminalMiddleware` 变为 `WorkspaceMiddleware` | ✅ **已实测** | `meta_harness::tests::{builtin_instance_policy_keys_match_declaration_table, known_key_union_covers_builtin_policy_keys, middleware_names_and_builtin_policy_keys_are_disjoint}` 绿（§3.6 02）；`assembly::tests::meta_harness_disables_each_known_middleware` 绿（§3.6 08）；`example/minimal/.peri/settings.json` 已改用 `"WorkspaceMiddleware": false`；配置加载链测试 `peri-acp/src/provider/{config_test,store_test}.rs` 随 §3.1 全量绿 |
| **S4** | 能力面**不引入** capability root | ⚠️ **如实登记为缺口（本波非交付项）** | `peri-middlewares/src/mcp/builtin/workspace.rs:11-16` 逐条登记；文件工具仍可访问 cwd 之外（`tools/filesystem/mod.rs` 的 `resolve_path` 无包含性检查）、Bash 仍无沙箱。**本记录不宣称已验证任何 capability root**（plan R8）。**补验（本轮）：见 §7 U3** —— 越界基线已取回（`Read`/上跳/`Write`/`Bash` 四条越界调用全部成功），根因登记为 wave 4（W4-1） |
| **S5** | 多 cwd 共享 host cwd（沿用既有退化） | ⚠️ **沿用既有裁决，无新增证据** | AW3-05：pool `execution_cwd` 为 `OnceLock` 一次性绑定；TUI/print/stdio 三路径的会话各自建环境池，多 cwd 场景未在本波做端到端验证（§7）。**补验（本轮）：见 §7 U4** —— 已闭合（实测 per-session 实例 / per-instance 单 cwd，两 session 互不串线） |
| **S6** | 工具声明模板内的**交叉引用裸名**与模型面名字不一致 | ✅ **已实测（逐字现场）** | 首个请求 system 文本三处逐字：① `Find files by name → \`mcp__workspace__Glob\` … not \`Bash\` with \`find\`; … list directories via \`folder_operations\` or \`Bash ls\`.`；② `List a directory / check structure → \`mcp__workspace__folder_operations\` … \`Bash ls -la\` for quick one-shot … avoid \`mkdir\`/\`test -d\` via \`Bash\`.`；③ `Run a shell command → \`mcp__workspace__Bash\` … Prefer the purpose-built tools **above** …`。模板其余占位符已正确渲染（`{{name}}` → effective name），**仅这三处硬编码裸名/顺序表述未改写**——按用户裁决「逐字保留，登记为已知项」，本波不改写 |
| **S7** | `timeout()` 语义变化：4 项由「外层 120s」变「桥内 `TOOL_CALL_TIMEOUT = 120s`」 | ⚠️ **代码事实成立；运行时未复现（UNVERIFIED）** | 代码事实：桥覆写 `timeout() -> None`（`peri-middlewares/src/mcp/tool_bridge.rs:234-236`）⇒ 外层不再设超时；桥内 `TOOL_CALL_TIMEOUT = 120s`（`:60`）包裹 `peer.call_tool`（`:278`）。**未做**「>120s 工具调用」的运行时复现（成本 2 分钟/次，本波未执行）⇒ 超时错误文本与取消归属差异**未见实测**，进 §7。**补验（本轮）：见 §7 U2** —— 已闭合（边界竞争桥胜；两条前提被推翻；另有两项新发现） |

**S3 附注 —— 计划口径与代码事实不符（U10 文档核对发现，已按事实更正）**：plan AW3-06（`spec/issues/2026-09-27-mcp-adaptation-v4-part-4-plan.md:71`）写「摘除后会被**拒收**」，并据此论证示例文件必须同批改键。**代码事实不是拒收，而是 warn 后移除（忽略）**：`peri-acp/src/provider/config.rs:391`（注释「MetaHarness 解析期校验（warn 不 fail）：未知 key warn + 移除」）、`:415-419`（`map.retain(...)` + `tracing::warn!("meta_harness: unknown key ignored")`）。⇒ 后果方向与计划相反：携带旧键 `"FilesystemMiddleware": false` / `"TerminalMiddleware": false` 的用户配置**不会报错**，只会得到一条 warn 且该键失效——关闭语义**静默丢失**（而非被拒绝），属需知情的语义变化。本波按事实口径把 `docs/code-index/peri-acp-types.md` 更正为「未知键 warn 后丢弃，不是拒收」（C4）；示例文件改键（`example/minimal/.peri/settings.json` → `"WorkspaceMiddleware": false`）的**动作**仍成立，只是计划给出的**理由**不成立；`"WebMiddleware": false` 在 `WorkspaceMiddleware` 落地前也曾退化为同一 warn 路径（`config.rs:402` 记载）。**UNVERIFIED（运行时）**：未做真实用户配置携带旧键的端到端复现（warn 文本与键失效为代码事实 + 结构测试，非端到端观测）。

**S1 之审批面明细（首个请求 system 文本，14 条敏感条目逐字）**：`mcp__workspace__Bash`（shell command execution）/ `mcp__workspace__folder_operations`（folder create/list/exists）/ `Agent` / `RunPtcCode` / `mcp__workspace__Write`（file write）/ `mcp__workspace__Edit`（file edit）/ `delete_*` / `rm_*` / `mcp__web__WebFetch` / `mcp__web__WebSearch` / `mcp__*`（前缀，附原文「builtin first-class capabilities follow their original tool's rule」）/ `DynamicMCP.load` / `DynamicMCP.unload` / `mcp__cron__cron_register` —— 计数 14、前缀条目 3、workspace 4 条为 effective name；`Read`/`Glob`/`Grep` **不在需审批清单**（正则 `^- \`(Read|Glob|Grep)\`` 命中 0），与 R5 一致，且与 `permission::tests::workspace_sensitive_entries_use_effective_names`（§3.6 07）互为旁证。

---

## §5 AW3-11 落地形态、1:N 形态与 `None` 退化登记

### §5.1 落地形态（与冻结设计的偏差）

落地为 **单槽 `HostAssemblyInput.workspace_input: Option<WorkspaceInstanceInput>`**，两名成员在该结构体内（plan §2 AW3-11 表项 1 的「两名成员槽」被实现为单槽内含两成员；语义等价，`peri-middlewares/src/mcp/builtin/context.rs:84-98`）。TaskManager 的**产生点上提**：`SessionEnvironment::assemble`（`peri-acp/src/host/workspace.rs:91-111`）先造 manager → 同一 `Arc` 同时进 `HostAssemblyInput` 与 `AcpSession`（`ensure_session_with_task_manager`，三条会话路径 `session_lifecycle.rs:190/:613/:1164` 同批）。`on_bg_complete` 由 `peri-agent/src/session/bg_complete.rs` 的 session 级工厂构造（lazy resolve inbox）。

**注入时序**：宿主注入位于 `peri-acp/src/host/assemble.rs:392-393`，**早于** `:496` 起 `run_initialize` 的 spawn。未做后置注入、未做 `instance_input_ready` 新 arm（`context::tests::instance_input_ready_truth_table` 覆盖「workspace 有无输入均 ready」）。

**同一性证据**（`host::workspace_seam::session_environment_holds_the_workspace_input_it_injected`，§3.6 15）：`WorkspaceInstanceInput.task_manager` ≡ `SessionEnvironment.task_manager()` ≡ 登记后 `AcpSession.task_manager`——三者 `Arc::ptr_eq`，且 `!is::<NoopTaskManager>()`。

**回调有效性证据**（`host::workspace_seam::session_bg_complete_callback_defers_into_the_registered_session`）：调用后目标 session 队列**恰 1 条** `Defer` / `MessageSource::ShellComplete`；`session::bg_complete::tests::{test_inbox_is_resolved_lazily_at_call_time, test_shell_completion_is_delivered_as_defer_with_shell_source, test_missing_inbox_is_silent_noop}` 覆盖 lazy resolve、投递形态与无 inbox 时的静默 no-op。

### §5.2 1:N 形态登记（`session_resources = true` 作 server root）

plan 取证已确认该形态**在装配面允许**（`mcp_v4_wave2_test.rs` 用 `true` 喂生产装配面并断言两 session 投影 `Arc::ptr_eq` 同一 host pool），但**今日生产无此调用方**（唯一 `true` 调用点是 `SessionEnvironment::assemble`，每 session 一次 ⇒ 生产四路径 session ↔ pool = 1:1）。该形态下的后果：builtin 上下文的 session 级槽位是**装配级一次性**，第二个 session 的输入注入得 `AlreadyInjected`（`assemble.rs` 仅 `tracing::error!` 记录、首个上下文继续生效）⇒ 该 session 的 builtin `Bash` 使用**首个 session 的** TaskManager（跨 session 串线），且**无错误码暴露给模型**。

**本波处置（如实登记，不作为已解决）**：本波未在装配处加「1:1 断言/fail-fast」，未把槽位改成 session 键控（计划明确列为非目标）。**登记为已知退化**；若未来出现把 `session_resources=true` 当 server root 的生产形态，必须先解决本项再放行（归 wave 4 议题）。

### §5.3 `None` 退化登记（`workspace_input: None`）

handler **照常构造、实例可见**（`dispatch` 无条件返回 `Workspace` 变体；`None` 时 `Bash` 无 manager / 无回调），退化逐条：

| 面 | `None` 时行为 | 证据 |
| --- | --- | --- |
| `run_in_background` | 直接拒绝（无 TaskManager 可注册） | `workspace_handler_without_session_input_rejects_run_in_background_over_wire`（§3.6 03，退化用例）；正向对照 `workspace_handler_bash_run_in_background_uses_injected_task_manager_over_wire` |
| 前台超时提升（promote） | 不提升，改杀进程组（`TimedOut` 而非 `RunningAfterTimeout`，无 `task_id`） | 本轮已运行时复现（U6-1，四例，见 §3.8）；**前提修正**：`terminal.rs:626` 不是 `None` 分支而是**提升被注册表拒绝**的回落分支（`:618-635`），`None` 退化分支是 `:637-661` |
| `command &` 残留子进程 | 不登记、无 `on_bg_complete` | U6-2 闭合（见 §3.8）：`Some` 有登记回执 / 注入 manager 上 `active_count()==1`；`None` 无回执且残留既未登记也未被连坐 |
| 完成回调 | 无 Defer、模型面无「后台 shell 完成」提醒（`registry.complete()` 照常 ⇒ 不卡住、不泄漏） | `test_missing_inbox_is_silent_noop`；plan §2.1 依据 4；**补 U7（见 §3.8）**：取消路径跳过回调已被断言锁定 —— 取消后回调计数 **0**、队列空、无 wake；自然完成对照恰 **1** 条 `Defer`/`ShellComplete` |
| 主链挂起场景（补验：U6-3 **结构性不可达**，见本行证据） | 主链因 `active_count > 0` 挂起 → 被 registry watch 唤醒 → 空转一次 Receive → turn 结束且不提任务（**与 workflow agent 路径的「有 manager、无 on_bg_complete」不完全同形**，不得简写为同形） | U6-3 **结构性不可达**：`None` 形态下 `active_count` 恒 0（`run_in_background` 在 `terminal.rs:345-347` 直接 `Err`、超时走杀组）⇒ 挂起无从发生；可挂起的 `Some(manager)+None(callback)` 形态生产不出现 ⇒ 留 wave 4（W4-5） |

**今日哪些路径实际落 `None`**：顶层三路径（TUI/print/stdio）与测试夹具的 `HostAssemblyInput` 字面量传 `None`（5 处 `None` / 1 处 `Some` = `workspace.rs:111`）；但生产四路径的**每个 session** 都经 `SessionEnvironment::assemble` 获得 `Some` ⇒ `None` 在当前生产不出现，仅存在于测试夹具与 1:N 退化形态。

## §6 归一闭合（N1–N10）与反例实验

**判定准则**（每条都是施工期实测，非推断）：① 破坏归一（改回裸名精确比较）⇒ 定向用例**必红**（记 `test result:` 行 + panic 原文）；② 逐字还原（`sha256` 与破坏前**逐字一致**）⇒ 回绿。**终态（未破坏）的绿证据**见 §3.6 定向批次（04/05/06/07/11/12/13/14/16 号日志）；本节引用的是**施工期**破坏实验的原始输出原文。终态**已三组重放**（N1/N2、N8、N3；逐条破坏/还原证据与 sha256 见 §7 U9），其余组（N4–N7、N9/N10）仍为施工期证据。

### §6.1 统一修法：唯一归一入口

全部 10 处失效点都走同一条「归一入口」——`peri_acp_types::builtin_mcp::original_tool_name_of_effective()`（判定/匹配型入口，plan AW3-07 第 1 条），分三种落法：

| 落法 | 用于 | 形态 |
| --- | --- | --- |
| 局部归一后比较 | N4–N10 | `let name = original_tool_name_of_effective(x).unwrap_or(x);` 再比较 |
| 单点封装 helper | N3 | `peri-middlewares/src/error_suggest/mod.rs:22` `pub(crate) fn normalized_tool_name(name: &str) -> &str`（`original_tool_name_of_effective(name).unwrap_or(name)`） |
| 双侧候选展开 | N1 / N2 | `peri-middlewares/src/subagent/mod.rs:390` `pub(crate) fn name_candidates()` + `:402` `pub(crate) fn declared_names_cover(declared, name)`（清单项与工具名**各自**展开后求交；与 `peri-agent/src/session/tool_catalog.rs:168` 的 `name_candidates` 同语义，因该函数为 crate 私有未能直接复用——plan 已预判该取舍） |

**保守语义不放松**：未命中归一（未知/外部 `mcp__*`、未注册实例、大小写不符）一律走**原有精确语义**——每条都有反例用例（`mcp__foo__*` / `mcp__workspace__NotATool` / `mcp__workspace__BashExtra`）钉住。

### §6.2 逐点落点与反例实验

| # | 终态落点（`仓库相对路径:行号`） | 具名用例 | 破坏实验必红证据（原文） | 还原 |
| --- | --- | --- | --- | --- |
| N1 | `peri-middlewares/src/subagent/fork.rs:58`（allowed 侧）、`:64`（disallowed 侧，均经 `declared_names_cover`） | `subagent::fork::tests::{explorer_disallow_matches_workspace_effective_name, coder_allowlist_matches_workspace_effective_name}` | `name_candidates` 退化为仅小写 ⇒ `FAILED. 24 passed; 2 failed`；`fork_test.rs:276`「裸名 `Write` 必须归一命中 `mcp__workspace__Write`（explorer 只读保证）」、`fork_test.rs:324`「白名单裸名 `Read` 必须命中 `mcp__workspace__Read`，实际：`["TodoWrite"]`」 | `fork.rs` sha 回 `10560153…` |
| N2 | `peri-middlewares/src/subagent/mod.rs:452`（`core_mutation_tools_fully_disallowed`）、`:478`（`ToolsValue::List` 分支） | `subagent::tests::{fully_disallowed_with_workspace_effective_names_is_readonly, capability_whitelist_effective_name_disallowed_by_bare_name_is_readonly, mutation_tool_matches_original_name_policy_for_builtin_names}`（后者原 `declared == 7` 硬编码已改注册表派生，A1 落地即红项已按 AW3-08 补记） | 同一破坏下 `FAILED. 32 passed; 2 failed`；`mod_test.rs:655`「effective name 写全五个核心写能力工具后应标 readonly（omitted tools 路径）」、`mod_test.rs:687`「白名单里的 `mcp__workspace__Write` 被裸名 `Write` 覆盖后应标 readonly」 | `mod.rs` sha 回 `26dd0271…` |
| N3 | `error_suggest/mod.rs:22`；5 个 suggester：`bash_command_suggester.rs:18`、`glob_pattern_suggester.rs:11`、`regex_suggester.rs:11`、`range_suggester.rs:13`、`path_suggester.rs:31`（白名单）与 `:81`（`path`/`file_path` 键分支） | `error_suggest::suggesters::*_test::test_*_matches_workspace_effective_name`（5 条正向）+ 5 条 `mcp__foo__*` 反例 | 破坏 `normalized_tool_name` ⇒ `FAILED. 34 passed; 5 failed`（5 条正向全红；5 条反例仍绿 ⇒ 保守语义未破）；单独破坏 `path_suggester.rs:81` 的键分支 ⇒ **仅** `test_path_suggester_matches_workspace_effective_names` 红（`path_suggester_test.rs:69`）⇒ 证明该分支必要 | `error_suggest/mod.rs` 回 `7c34cb7e…`；`path_suggester.rs` 回 `4b133b50…` |
| N4 | `peri-tui/src/kit/tool_display.rs:17`（`unwrap_or(raw)`；未命中别名回落**传入名原样**，不降级显示） | `kit::tool_display::tests::workspace_effective_names_map_to_short_names` | 去归一 ⇒ `panicked at tool_display_test.rs:221: left: "mcp__workspace__Bash" right: "Shell"` | `tool_display.rs` 回 `9e4616b3…` |
| N5 | `peri-tui/src/kit/message_area/render/tool_card.rs:268`（`$ command` 前缀门控）、`:455`（`completed_header_suffix` 后缀） | `kit::message_area::render::tests::{workspace_bash_card_keeps_dollar_prefix, workspace_read_and_edit_cards_keep_header_suffix}` | 去归一 ⇒ 2 条同红（`render_test.rs:3822`「workspace Bash 展开态应有 `$ command` 行，实际：`" ▾  Shell cargo test …"`」、`:3849`） | `tool_card.rs` 回 `52a813fc…` |
| N6 | `peri-tui/src/kit/acp_types/tool_card.rs:68`（`tool_path_hint`）、`:90`（`parse_tool_diff`） | `kit::acp_types::tests::workspace_edit_builds_diff`（含 path hint 与 `adds/dels`） | 两处同破 ⇒ `panicked at acp_types_test.rs:804: mcp__workspace__Edit 完成态必须解析出 diff`；**单独**破 `tool_path_hint` ⇒ `acp_types_test.rs:805: left: "render.rs" right: "peri-tui/src/render.rs"` ⇒ 两处各自独立承重 | `tool_card.rs` 回 `e3addcdd…`（第二次实验后同样回到该 sha） |
| N7 | `peri-tui/src/kit/acp_types/current_turn.rs:263`（递归分支复用同函数，未改） | `kit::acp_types::tests::has_running_bash_tool_matches_workspace_effective_name` | 去归一 ⇒ `panicked at acp_types_test.rs:844: mcp__workspace__Bash 运行中必须命中` | `current_turn.rs` 回 `7ab442d7…` |
| N8 | `peri-middlewares/src/permission/mod.rs:149` / `:154` / `:169` / `:174`（**原地改名** 4 项，`builtin_tool_effective_name("workspace", …)`；`:189`/`:194`/`:214` 为 wave 1/2 既有改名，同一范式） | `permission::tests::{workspace_sensitive_entries_use_effective_names, sensitive_entries_match_default_requires_approval, sensitive_entries_cover_all_requires_approval_branches, hitl_section_declaration_shape}` | 退化为 `original_name` ⇒ `FAILED. 30 passed; 4 failed`；`mod_test.rs:793`「条目名不应再是裸名 `Bash`（该名字迁移后不存在）」+ `hitl_section_declaration_shape` + `sensitive_entries_use_effective_names_and_parity_description` + `cron_sensitive_markdown_uses_registry_effective_name` | `permission/mod.rs` 回 `3f4e594f…` |
| N9 | `peri-acp/src/session/command/rewind.rs:256`（OpenAI `tool_calls` 路径）、`:276`（Anthropic `ContentBlock::ToolUse` 路径）；下游 `parse_tool_call` **保持纯**（只接原始名，注释锚在 `:250`） | `session::command::rewind::tests::{rewind_collects_workspace_effective_write_and_edit, rewind_ignores_unknown_effective_names}` | 见 §6.3 的四段（A/B/C/D） | `rewind.rs` 回 `1c1a83b8…` |
| N10 | `peri-agent/src/agent/compact_v2/full.rs:420`（`extract_recent_files`）、`:454`（`extract_skills_paths`） | `agent::compact_v2::full::tests::{extract_recent_files_matches_workspace_effective_read, extract_skills_paths_matches_workspace_effective_read}` | 见 §6.3 | `full.rs` 回 `25bc4257…` |

**N8 的裁决合规**：plan §3.2 N8 的「原地改名、**不是新增**」已逐字遵守——条目数仍 **14**、前缀条目仍 **3**（`sensitive_entries_cover_all_requires_approval_branches` 未放宽）；`Read`/`Glob`/`Grep` 的免审批语义以**反向断言**呈现（`sensitive_entries_match_default_requires_approval` 的反向抽查 + `workspace_sensitive_entries_use_effective_names` 断言三者**不在**清单且判定仍为免审批）。

### §6.3 N9/N10 的四段反例实验（隔离验证，含一次**测试自身缺陷**的发现）

| 段 | 破坏内容 | 必红原文 | 结论 |
| --- | --- | --- | --- |
| A | 四个调用点全部去归一 | acp `FAILED. 35 passed; 1 failed`：`rewind_test.rs:312: 'mcp__workspace__Write' 应被归一为 Write 并收集`；agent `FAILED. 167 passed; 2 failed`：`full_test.rs:877`、`:915` | 归一为承重件 |
| B | **过度归一**（`rsplit("__").next()`，两文件各两处） | acp：`rewind_test.rs:371: 'mcp__foo__Write' 未命中归一表，不得被收集（保守语义）`；agent：`full_test.rs:897`、`:931` | **保守语义**是断言面的一部分，不允许「见 `mcp__` 就剥前缀」 |
| C | **仅** ToolUse 路径去归一 | **首跑仍绿** —— 暴露测试弱点：`ai_from_blocks` 会把 ToolUse 同步进 `tool_calls`，OpenAI 路径先收集并掩盖 ToolUse 路径的失效 | 已把两格式断言改为 **blocks-only 构造**（`BaseMessage::ai(MessageContent::Blocks(...))`、`tool_calls` 留空，测试内注明原因）后再跑：`rewind_test.rs:343: left: 0 / right: 1` 红 | 断言改造使**两条格式路径各自可失败** |
| D | **仅** ToolUse 路径过度归一 | `rewind_test.rs:386: 'mcp__foo__Write'（ToolUse 路径）未命中归一表，不得被收集` | 同上，隔离到具体路径 |

**A/B/C/D 四段还原后 sha 与破坏前逐字一致**（`rewind.rs` = `1c1a83b8…`、`full.rs` = `25bc4257…`），且工作树无 `BREAK-` 残留标记。

### §6.4 与 plan §3.2 的偏差（须登记）

1. **N2 未单独复制 helper**：plan 措辞为「与 N1 同一归一策略」；落地为 N1/N2 共用 `subagent/mod.rs` 的 `name_candidates` + `declared_names_cover`（N2 的两处调用点改为该 helper），**未**在 `fork.rs`/`mod.rs` 各复制一份。理由：同一 crate 内共用一份实现符合「业务规则单一权威」；plan 未排除该形态。
2. **N3 的 helper 是 `pub(crate)`**（非 plan 措辞中的自由函数说明）：作用域限于 `error_suggest` 内部，避免扩大公开面。
3. **`name_candidates` 未提升为 `pub`**（`peri-agent/src/session/tool_catalog.rs:168` 保持 crate 私有）：plan 把它列为可选项，未采纳（跨 crate 复用会引入 `peri-middlewares → peri-agent` 的额外耦合面，而 `peri-middlewares` 已直连 `peri-acp-types` 可自足）。

---

## §7 UNVERIFIED 清单（运行时未成立的一切都在这里，不写进上文断言）

**口径**（plan R8）：本节每条都是「代码事实/设计归属成立，但**未取回运行时证据**」或「本波**未落地**」。§1–§6 中出现 ⚠️ / 引用本节编号的断言，均以「不见实测不给 ✅」为准。

| # | 未验证项 | 为什么未验证 | 影响 | 补验方式 |
| --- | --- | --- | --- | --- |
| U1 | **S1 观测面⑥ Langfuse 观测名**：工具名在 Langfuse trace 中的呈现<br>**本轮结论（运行时证据）：闭合** —— tool observation 的 `name` 逐字 = 模型面 effective name（`mcp__workspace__Write` / `mcp__workspace__Read` / `mcp__workspace__Bash`）；原「若取自 `BaseTool::name()` 则仍是裸名」的分支**已不成立**（`McpToolBridge::name()` 返回的就是 effective name）⇒ 两分支收敛为同一件事 | 运行时未接 Langfuse（无凭据/未开观测）<br>**前提修正**：未验证原因**不是**「无凭据」，而是「本地 Langfuse 实例 `localhost:23332` 未运行」 | 若观测名取自模型面工具名，则 S1 同样生效；若取自 `BaseTool::name()` 则仍是裸名——**本记录不宣称**<br>**残余**：① 服务端/UI 是否再归一无法观测（真实服务不可达）；② 真实凭据链路下的鉴权行为未验证（本轮刻意用 dummy 凭据，与 `name` 字段无关）；③ 相邻风险：`peri-controller/src/langfuse/tracer/tool_events.rs:54` 与 `tool_batch.rs:88` 用 `name == "Agent" \|\| name == "Task"` 判 agent 工具——Agent/Task 本波未迁移故成立，**将来若迁为 `mcp__*__` 该比较会静默失效**（→ W4-6） | 在开启 Langfuse 的构建上跑一次 print smoke，核对 trace 内 tool name 字段<br>**证据指针（本轮已补验）**：`/tmp/u1_lingering/FINDINGS.md`（完整链路表）；`/tmp/u1_lingering/run/otlp/req_001.body.json`（本地 OTLP 捕获的**真实二进制载荷**，633057 bytes；`tool-type observations = 3`，三条名字逐字如上；batch span 的 `input.tools` 同样为 `["mcp__workspace__Write"]`）；采集命令原文 `bash /tmp/u1_lingering/run_u1.sh` → `exit=0 model_port=61420 capture_port=61418 requests=5 otlp_bodies=1`；方法说明：`LANGFUSE_*` 五变量存在且非空，但 `LANGFUSE_BASE_URL` 是 loopback 且本地实例未运行（`curl` exit 7）⇒ 改用本地 OTLP 捕获服务（dummy 凭据）。取值链逐跳：`tool_bridge.rs:218-220`（`full_name` 由 `:103-109` 构造）→ `peri-agent/src/tools/invocation.rs:50-54` → `agent/stages/tool_dispatch.rs:160-168` → `execution.rs:178-185` → `render_event/v2_conversion.rs:26-37` → `peri-controller/src/langfuse/tracer/bridge.rs:166` → `tool_events.rs:27/58/208` → `tool_batch.rs:93/133`；Langfuse 侧**不做**自己的归一（`types/conversion/observation.rs:144` 逐字拷贝） |
| U2 | **S7 超时语义的运行时行为**：>120s 工具调用的错误文本、取消归属、与 Bash 前台 120s 上界的**边界竞争**<br>**本轮结论（运行时证据）：闭合，但原设想的三条前提被推翻两条** —— 探针 A′（`{"command":"sleep 150","timeout":120000}`）与 B（`timeout:600000`）均在 **120.026s / 120.025s** 返回桥文本 ``MCP 服务器 "workspace" 工具 "Bash" 调用超时 (120s)``，**从未**出现 Bash 侧文案 ⇒ **边界竞争：桥胜**（桥计时起点 `tool_bridge.rs:278` 恒早于 Bash 的 `terminal.rs:493`） | 未做单次 ≥120s 的运行时复现（成本高、本波未执行）<br>**前提修正 1**：`{"command":"sleep 150"}`（**无** `timeout` 字段）实为 **15.046s** 返回（`FOREGROUND_DEFAULT_TIMEOUT_MS`），**不是**「工具自身默认 120000ms」——探针 A 时间线：send `1790515124149` → recv `1790515139195`。**前提修正 2**：`timeout:600000` **不能**把 Bash 前台超时抬到 10 分钟 —— `parse_foreground_timeout` clamp 回 `FOREGROUND_MAX_TIMEOUT_MS`（`timeout: 0` 亦同）⇒「隔离桥内 120s」的构造法**结构性不可达**（探针 B ≡ A′） | 4 个文件工具的超时从「外层」移到「桥内」，错误语义可能不同；Bash 两处 120s 可能同时到期<br>**新发现（须登记）**：① **错误文本被 IF-D14 覆盖** —— `BashTool::invoke` 把 `TimedOut \| RunningAfterTimeout` 转 `Err(text)`（`terminal.rs:756-773`），workspace handler 的 `Err` 分支把它整段丢弃、换成固定脱敏文本（`web.rs:39-44` + `:106-112`）⇒ 模型面**拿不到** `task_id`/`pid`/部分输出/`timeout_note`/处置指引（原生链路对照 `execution.rs:319/351-366` 保留真实文本）→ W4-3；② **桥超时不取消内层调用** —— 120.026s 返回桥错误后，内层 Bash 仍跑完自己的 120s 期限并**把进程提升为后台任务**（哨兵 `leftover-sleep.txt`：进程存活至 59s、自成一进程组），会话因此被 `active_count > 0` 挂到 **+150.13s** 才继续（两次独立观测 turn2→turn3 间隔 **29.994s / 29.998s**）⇒ 模型被告知「调用超时」，而后台任务其实存在且**无 `task_id` 可引用** → W4-4 | 构造 `sleep 150` 探针，记录错误文本与取消路径归属<br>**证据指针（本轮已补验）**：`/tmp/u238_lingering/{U2A,U2A2,U2B}/`（各含 `cmdline.txt`、`scenario.json`、`timeline.ndjson`、`run.stamped`、`reqs/req-N.json`、`exit-code`、`wall_secs`）；报告脚本 `u2_report.js`；探针 B `exit=0`、wall 155s、`result` 事件齐全。**残余 R1**：未观测到「无任何内层期限、纯桥 120s」的隔离形态（构造法被 clamp 破坏，替代构造未执行）。**残余 R2**：4 个文件工具的表述本身需修正 —— 迁移前 7 个工具 `timeout()` 全为 `None`（Write `write.rs:183-185`、Grep `grep.rs:461-463` 显式 `None`，其余走 trait 默认），外层 `timeout_opt = tool.timeout()`（`execution.rs:296`）**从未生效**；迁移后桥内**新增** 120s 硬上界 ⇒ 原表述「从外层移到桥内」应改为「**新增桥内 120s 上界**」 |
| U3 | **AW3-04 capability root**：文件工具仍在 cwd 之外可达、Bash 无沙箱<br>**本轮结论（运行时证据）：闭合 —— 越界可达性 = 是（三条独立路径全部成立，共四条调用）**：`mcp__workspace__Read {"file_path":"/etc/hosts"}` ⇒ 读到 cwd 外；`Read {"file_path":"../../../../../../etc/hosts"}` ⇒ 成功上跳（与前者逐字相同）；`Write {"file_path":"/tmp/u238_lingering/outside_write.txt"}` ⇒ `Wrote 1 line /private/tmp/u238_lingering/outside_write.txt`；`Bash {"command":"ls / \| head -3"}` ⇒ `Applications\nbin\ncores\n` | 本波**明确不引入**新根（AW3-03/AW3-04 裁决）；**也未**做越界路径探针（未构造 `../` 或绝对路径用例）<br>**前提修正（范围收窄）**：原「未做越界探针」**已消除**（基线已取回）；「本波不引入新根」的裁决**不变** ⇒ 设计文档的「独立 capability root」属 **wave 4 议题（W4-1）**，非本波待办 | 设计文档 `:31`/`:163` 的「独立 capability root」在本波**未落地**，属已知缺口<br>**残余**：无新增缺口项（根因已登记为 wave 4） | wave 4 设计时先补越界探针作为基线，再引入 root<br>**证据指针（本轮已补验）**：`/tmp/u238_lingering/U3/run.stamped`（5 次脚本化调用全表），`exit=0`、wall 7s；旁证 `ls -la /tmp/u238_lingering/outside_write.txt` 存在且内容 `w3-u3-marker`。**代码锚点（无任何 root/前缀校验）**：`peri-middlewares/src/mcp/builtin/workspace.rs:11-16`（注释明示不引入 root）；`web.rs:89-112`（cwd 原样透传 `ToolContext::new(&[], cwd)`）；`tools/filesystem/read.rs:178-227`（`resolve` → `read_to_string` 无前缀检查）；`middleware/terminal.rs:420-431`（仅 `current_dir(&self.cwd)`） |
| U4 | **AW3-05 多 cwd**：多 session 共享 host cwd 的端到端表现<br>**本轮结论（运行时证据）：闭合** —— 每个 session 的 workspace 实例 cwd = 该 session 在 `session/new` 声明的 cwd（canonicalize 后），互不串线。逐字（按 `toolCallId` 归属）：session a `pwd` ⇒ `"/private/tmp/u4_lingering/a\n"`、`Read only-a.txt` ⇒ `ONLY_A_CONTENT`、`Read only-b.txt` ⇒ failed（脱敏文本）；session b 对称。每 session 各一条 `system-reminder-fallback` 文本 `MCP: 5 connected, 0 failed, 0 disabled` | 未构造「同 host 内两个不同 cwd 的 session 并发」场景<br>**目标归属文本需重述**：AW3-05 裁决文本的「per-host 单 cwd」与代码事实不符 —— 实现是 **per-session 单 cwd**（`SessionEnvironment::assemble` 用 session 自己的 cwd 建**独立** MCP 池 + builtin 上下文）⇒ 建议重述为「**per-session 实例 / per-instance 单 cwd**」（`peri-acp/src/host/requests/session_lifecycle.rs:523-532`/`:569-570`、`host/workspace.rs:49-53`/`:108`、`host/assemble.rs:271` 顶层传 `false` ⇒ `:668` 的 `workspace_assembly = Some(..)`） | 属既有裁决的沿用退化（wave 2 LSP 同口径）<br>**残余 4 条**：① `/mcp` 池面板在 stdio 下不可达（`available_commands_update` 9 条不含 `mcp`）⇒「两个 workspace 实例」的直接内窥未取得；② 仅实测 `Read`（相对路径解析）与 `Bash`（`current_dir`），Write/Edit/Glob/Grep/folder_operations 未逐一验证；③ TUI 路径未实测（以 ACP stdio 替代）；④ 环境事实：会话中途 `target/debug/peri` 被外部重建，另有外部 agent 写入 `workspace_test.rs` 的 TEMPORARY PROBE（U6 侦察）——**提交前已确认清除**（2026-09-27 提交批次全 `src` 检索 `TEMPORARY PROBE` 零命中；该文件的未提交改动为 U6/U7 正式用例 6 例 + U5 补验用例） | 在 TUI 内开两个不同 cwd 的 session，核对文件工具的实际作用域<br>**证据指针（本轮已补验）**：`/tmp/u4_lingering/U4-EVIDENCE.md`；`/tmp/u4_lingering/run.sh main "a=/tmp/u4_lingering/a,b=/tmp/u4_lingering/b"` → `run=main exit=0 requests=6`；`triple` → `exit=0 requests=9`（host cwd = `…/hostroot`，与 a/b 都不同）；产物 `main/{stdout.ndjson,analysis-tools.txt}` 与 `triple/` |
| U5 | **§5.2 的 1:N 形态**：N 个 session 共享一份 builtin 上下文时，第二个 session 的输入注入得 `AlreadyInjected`、首个上下文继续生效<br>**本轮结论（运行时证据）：判定反转（原判定在 1:N 形态下不成立）** —— 该形态下**不存在**「第二个 session 的注入尝试」：任何 session 调 `SessionEnvironment::assemble` 都返回 `Ok(None)`（首句 `host.workspace_assembly.as_ref()` 取不到 `Some` 即早返回）⇒ 也**不会**得 `AlreadyInjected`（那只由同一 pool 被重复 `set_builtin_instance_context` 触发，由 `mcp_v4_wave2::context_injected_before_initialize_and_rejects_duplicate` 覆盖，与本形态无关）。真退化是：**root 那一份 `WorkspaceInstanceInput` 被全体 session 冒充使用**，而每个 session 记录里认领的 `task_manager` 由会话工厂各造一份、与实例里那份无关；两者不等**全程静默**（不产生任何面向客户端的错误）。逐字现场打印（`--nocapture`；session id 每次运行不同）：`[W3-U5 补验] 1:N root：workspace_assembly=None、mcp_pool=Some；两个 cwd 的会话环境装配 = Ok(None)；session = ["01a0e33a-2d42-7972-a36f-9555911df360", "01a0e33a-2d66-7a12-95a7-4e54132090e5"]（共享同一 host pool = true）；root 输入 manager 与两个 session 的 manager 均非同一份 = true（两 session 之间也互不相同 = true）` | 该形态**今日无生产调用方**（唯一 `true` 调用点每 session 一次）<br>**前提修正**：「本波未在运行时复现」已消除——本轮已用 `true` root 夹具在运行时复现；原判定「第二个 session 注入得 `AlreadyInjected`」**不成立**（机制见左列） | 若未来出现该形态，会出现「跨 session 串线且无错误码暴露」<br>**残余**：该形态今日无生产调用方；串线静默、无错误码（→ W4-2 的 1:1 fail-fast 断言） | 用 `mcp_v4_wave2_test.rs` 式的 `true` root 夹具开两个 session，断言第二个 session 的 `Bash` 后台任务归属<br>**证据指针（本轮已补验）**：新用例 `peri-acp/src/host/mcp_v4_wave2_test.rs:668`（`wave3_one_to_n_root_never_injects_per_session_workspace_input`，4 条各自可证伪的断言：形态前提 / 恒 `Ok(None)` / 同一 host pool `Arc::ptr_eq` / 两份 manager `!Arc::ptr_eq`）+ 夹具 `:433` + 现场打印 `:811`；`cargo test -p peri-acp --lib -- host::mcp_v4_wave2` → `ok. 14 passed; 0 failed; 725 filtered out; finished in 48.00s`，`EXIT=0`（`/tmp/u5_verify/acp_wave2.log`）；单例 `--nocapture` ⇒ `ok. 1 passed; 0 failed; 738 filtered out; finished in 0.20s`，exit=0。**可证伪性（本轮）**：`peri-acp/src/host/assemble.rs:668` 的 `(!session_resources)` 改为 `true` ⇒ 必红 `panicked at mcp_v4_wave2_test.rs:688:5: 本用例只在 1:N 形态成立…`（`FAILED. 0 passed; 1 failed; 738 filtered out; finished in 0.14s`，exit=101）⇒ 逐字还原 sha256 `97224fd0…` 一致 ⇒ 回绿 |
| U6 | **§5.3 `None` 退化的三条运行时行为**：超时不提升为后台任务 / `command &` 残留子进程不登记 / 主链「挂起→唤醒→空转→turn 结束且不提任务」的时序<br>**本轮结论（运行时证据）：部分闭合** —— U6-1（前台超时 promote 对照）**闭合**（两层证据：进程级哨兵文件落盘与否；工具层文本 / 状态 / `task_id` 三处互斥）；U6-2（`command &` 残留登记对照）**闭合**；U6-3 仍 UNVERIFIED 但已论证**结构性不可达**（⇒ wave 4） | 均为代码分支与计划的静态取证；未在真实二进制上构造 `None` 形态复现（生产四路径恒为 `Some`，无法从外部触发）<br>**前提修正**：`terminal.rs:626` **不是** `None` 分支，而是**提升被注册表拒绝**的回落分支（`:618-635`，`SHELL_LIMIT` 满）；`None` 退化分支是 **`:637-661`**（本轮新增第 4 个用例专测被拒分支）。U6-3 不可达的机制：`None` 形态下 `active_count` 恒 0（`run_in_background` 在 `terminal.rs:345-347` 直接 `Err`、超时走杀组） | 只影响测试夹具与 1:N 退化形态<br>**残余**：① U6-1 的「5 个真 shell 并发」形态未跑（用占位条目制造 `SHELL_LIMIT` 饱和，分支选择只看 `register` 返回，等效）；② 线路层「提升 vs 杀组」不可区分是既定脱敏行为（已断言锁定），若后续希望模型面感知提升属产品决策；③ U6-3 复现需 agent-loop 级夹具 ⇒ 留 wave 4（W4-5） | 单测级：以 `workspace_input: None` 构造池，跑 `sleep 150` + `cmd &` 两探针，核对 `TimedOut` 与无 Deferred 消息<br>**证据指针（本轮已补验）**：`peri-middlewares/src/mcp/builtin/workspace_test.rs:582`/`:687`/`:778`/`:859` 四例 + 破坏/还原台账 5 次（`/tmp/u67_lingering/break{1,2,3,4a,4b,5}_run.log`，生产文件终态 sha 与基线一致：`terminal.rs dc78796f…`、`shell.rs 31432329…`）；`cargo test -p peri-middlewares --lib -- mcp::builtin::workspace::tests` → `ok. 11 passed; 0 failed`（3 连跑，`/tmp/u67_lingering/targeted_FINAL.log`）；详见 §3.8 |
| U7 | **取消路径上的 `on_bg_complete`**：`shell.rs` 的 `claim_completion` 短路使取消路径**按设计跳过回调**<br>**本轮结论（运行时证据）：闭合** —— 真 `TaskManager::spawn_shell` + 真 session 级回调 + 真 `SessionInbox`/`MessageQueue`：取消后回调计数 **0**、队列 `len()==0`、无 wake；同一夹具的自然完成对照为 **1** + 1 条 `Defer`/`ShellSource`（`shutdown()==Complete` 作为确定性屏障） | 代码事实（`peri-agent/src/agent/async_tasks/shell.rs:588-590`）；本波只断言「自然完成」路径送达<br>**前提修正**：「只断言自然完成」已扩展为**两路径均有断言**（取消路径 + 自然完成对照），取消路径的跳过行为由断言锁定而非仅代码事实 | 后台任务被取消时不产生 Defer / 提醒——与迁移前链上行为一致，非本波引入<br>**残余**：`peri-acp` 宿主侧（`host/workspace_seam_test.rs`）只覆盖自然完成投递，**未覆盖取消** ⇒ 如需可后续补端到端取消探针（→ W4-10） | 单测：发起 `run_in_background` 后取消，断言队列无 `Defer`<br>**证据指针（本轮已补验）**：`peri-agent/src/session/bg_complete_test.rs:235`（`test_cancelled_background_shell_skips_completion_callback_and_defer`）、`:305`（`test_natural_background_shell_completion_delivers_defer_through_the_manager`）；`cargo test -p peri-agent --lib -- session::bg_complete::tests` → `ok. 6 passed; 0 failed`（3 连跑，`/tmp/u67_lingering/targeted_FINAL.log`）；破坏日志 `break4a/4b_run.log`；详见 §3.8 |
| U8 | **`namespace()` 未转发导致声明段排序退化**：桥不转发 `namespace`（trait 默认 `None`）⇒ 7 项在声明段按空前缀参与 `(namespace, name)` 排序<br>**本轮结论（运行时 + 静态）：原判定被部分推翻（判定反转）** —— ① **顺序前后一致（原判定错）**：四面（① `RunPtcCode` 段内目录 JSON、② 搜索面 CSV、③ system 声明段模板行 L438–L454、④ 模型 `tools[]`）的 7 项**相对顺序前后完全一致**，绝对位置也一致（L441–L447），**索引 diff = 0**；机制：迁移前键为 `("", "Bash")` + `("filesystem", …)`，迁移后桥不转发 `namespace`（`McpToolBridge` 未覆写，`peri-acp-types/src/tools.rs:626-628` 默认 `None`）⇒ 7 项全落**空前缀组**，组内 `mcp__web* < mcp__workspace*` 与旧顺序**恰好同形**。② **「7 项文本不变」不成立（新发现）**：6/7 模板正文含额外 `{{name}}`（Glob 4 处、其余各 2 处；Bash 仅 1 处）⇒ 迁移后正文里的裸名也变成 effective name（例：``never `Glob("*")`/`Glob("**/*")` `` → ``never `mcp__workspace__Glob("*")`/…``）——这是**设计必需**（否则会指挥模型调用已不存在的裸名） | 仅静态判定（7 个模板均不含 `{{namespace}}` ⇒ 文本不变、仅**顺序**可能不同）；未做改动前后的顺序逐字 diff<br>**前提修正**：原「仅顺序可能不同、内容不变」应改为「**顺序不变（索引 diff = 0）、内容（模板正文裸名）确有变化且属必需**」；`{{namespace}}` 计数 7/7 = 0 该半**成立**（锚点 `peri-acp-types/src/builtin_mcp.rs:138/146/154/162/170/178/186`，逐行 `{{name}}` 计数 2/2/2/4/2/2/1） | 系统提示中 7 项的**相对顺序**可能变化（内容不变）<br>**残余 / 登记**：① `namespace()` 转发议题本身保留（→ W4-7，降级为「分组排序退化」，文本不受其影响）；② **顺带发现**：首个请求的 `connection_summary` 存在竞态 —— U2A 的 req-1/req-2 写 `MCP: 3 connected`（只列 artifact/cron/lsp），**同一请求的 `tools[]` 却有 17 项含全部 7 个 `mcp__workspace__*`**；A′/B 同位置为 `5 connected`。触发条件未构造，仅登记（→ W4-8） | 对同一 smokes 的 system 文本做改动前后 7 项出现序的位置 diff<br>**证据指针（本轮已补验）**：`/tmp/u238_lingering/u8_faces.js` / `u8_decl.js` / `u8_verbatim.js` / `u8_face3.js`；产物 `/tmp/w3_smoke3/W3/reqs/req-1.json`（迁移后）vs `/tmp/w3_tui_smoke/tui/reqs/req-1.json`（迁移前） |
| U9 | **N1–N10 反例实验的终态未重放**：§6 引用的是施工期破坏实验的原始输出；终态（未破坏）的绿证据是另一批日志<br>**本轮结论（运行时证据）：三组已在终态重放（判定收窄）** —— ① **N1/N2 组**（`peri-middlewares/src/subagent/mod.rs:390` 的 `name_candidates`）：基线 `subagent::fork::tests` `ok. 26 passed` / `subagent::tests` `ok. 34 passed`；破坏（body 退化为 `vec![name.to_lowercase()]`，sha256 `0caf190a…`）⇒ `FAILED. 24 passed; 2 failed`（panic `fork_test.rs:324`「白名单裸名 `Read` 必须命中 `mcp__workspace__Read`」、`fork_test.rs:276`「裸名 `Write` 必须归一命中…（explorer 只读保证）」）与 `FAILED. 32 passed; 2 failed`（`mod_test.rs:655`、`:687`）；还原 sha256 `26dd0271…` 逐字一致 ⇒ 回绿。② **N8 组**（`permission/mod.rs`）：基线 `permission::tests` `ok. 34 passed`；调用点级破坏（4 处条目名退回裸名，sha256 `b206291f…`）⇒ `FAILED. 32 passed; 2 failed`（`mod_test.rs:507`、`:793`）；解析助手级附加诊断（`builtin_tool_effective_name` 退化为裸名，sha256 `195d43a5…`）⇒ `FAILED. 30 passed; 4 failed`（补上 `:736`、`:867`）⇒ 归因：施工期记录的「3–4 红」对应**解析助手级**破坏点（调用点级只红 2，为其子集，非断言放宽）；还原 sha256 `3f4e594f…` 一致 ⇒ 回绿 + `peri-middlewares --lib` sanity `ok. 1990 passed; 0 failed; 5 ignored`。③ **N3 组**（`error_suggest/mod.rs:22` 的 `normalized_tool_name`，本轮补做）：基线 `error_suggest::` `ok. 39 passed`；破坏（body → `name`）⇒ `FAILED. 34 passed; 5 failed`（5 条 `*_matches_workspace_effective_name`；panic 例 `bash_command_suggester_test.rs:86`、`glob_pattern_suggester_test.rs:67`、`path_suggester_test.rs:69`、`range_suggester_test.rs:106`）⇒ 还原 sha256 `7c34cb7e…` 一致 ⇒ 回绿 `ok. 39 passed; 0 failed` | 破坏实验会短暂污染工作树，终态为避免并发写者互相干扰未再重放<br>**前提修正**：「终态未重放」在**三组**上已消除（重放窗口内工作树含并发写者产物，但被破坏的三文件均不在 `git status` 中，且终态 sha256 与基线逐字一致 ⇒ 干扰面为 0） | 「破坏必红」的证据链原先全是施工期观测 + 还原 sha 一致，**不是**终态重放<br>**残余**：N4–N7（peri-tui 四点）与 N9/N10（rewind/compact）仍为施工期证据（§6.2/§6.3），未做终态重放 | 如需，可在干净副本上重放 §6 的破坏/还原（每点 ≤2 条定向命令）<br>**证据指针（本轮已补验）**：`/tmp/u9_lingering/{baseline_all.log,g1_broken.log,g1_regreen.log,g2_broken.log,g2_regreen.log,g2x_resolver_broken.log,g3_broken.log,g3_regreen.log}` + 三个基线 sha256（`10560153…` fork.rs / `26dd0271…` subagent/mod.rs / `3f4e594f…` permission/mod.rs）+ 备份 `/tmp/u9_lingering/*.bak`；终态三文件 sha256 复核一致 |
| U10 | **文档面（C4）内容的逐行正确性**：`docs/**` 的 12 个文件 + `example/minimal/**` 已按 DOC-UPDATE-001 同步（文件级 diff 已核，见 §8-9）<br>**本轮结论（静态审计 + 修复）：闭合** —— 审计完成 + 修复完成（未提交）：62 个文档给出的 `path:line` 锚点中 **56 命中**；偏差 **14 项全低危**（陈旧锚点 1 组 6 个 / 陈旧计数 1 / `#[cfg(test)]` 未标注 1 / 措辞过强 3 / 因果反了 1 / 类别定义未覆盖第五键 1 / 指针不精确 1 / 模块不变量缺边界 1 / 策略键枚举漏项 1 / 非 12 文件的陈旧引用 3） | 本记录只做**文件级**核对（哪些文件改了、改了多少行），未逐行核对每处描述与代码事实一致<br>**前提修正**：原判定已被取代为「**逐行审计已完成**」；审计模式为**只读**（未修改任何文件，结论全部来自 Glob/Grep/Read 静态阅读） | 文档可能与实现细节有偏差<br>**残余（未改项，登记）**：`docs/peri-internal-architecture.html:3156`（禁改）；`spec/**`（冻结）；`docs/design/tool-system.md:90`（不在清单）；`docs/design/meta-harness.md:261`（同款可证伪措辞，本轮判为「正确」但复核发现同类问题 ⇒ 建议补登 wave 4）→ W4-9 | 按 `DOC-UPDATE-001` 的核对清单逐文件复核<br>**证据指针（本轮已补验）**：审计报告 `/Users/konghayao/code/ai/peri/.peri/plans/u10-doc-line-audit.md`（同树副本 `.peri/plans/u10-doc-line-audit.md`）；修复批次 9 文件 / 15 处：`docs/design/middleware-system.md`（`:107` 24→22；`:146` 补 `LspMiddleware`）、`docs/code-index/peri-middlewares.md:64`（6 锚点）、`docs/meta-harness.md:43-48`（因果 + 措辞）、`docs/reference/mcp-ecosystem.md`（`:581` 指针、`:727` 措辞）、`docs/code-index/peri-acp.md:16`（`#[cfg(test)]` 标注）、`example/minimal/README.md`（`:19`/`:40-44`/`:67`）、`peri-agent/CLAUDE.md:32`、`docs/design/git-watch-middleware.md:126`、`docs/design/workflow.md:363`。**注意**：修复批当时 `cargo fmt --all --check` 因**他人文件**（`workspace_test.rs`）红，提交前须复核 fmt 已恢复绿 |
| U11 | **记录快照的可复现性**：§0 的树指纹（`git diff HEAD \| shasum`）与 97 项计数是**某一时刻**的快照<br>**本轮结论（树指纹重取）：已按新时点重取并登记口径** —— §0 的 97 项快照**保留为「记录初版快照」**；现取 `git status --porcelain` = **12 项**（提交 `1411ed39` 已含 90+ 项；现存 12 项 = U10 修复 9 项 + U6/U7 施工 3 项文件），`git diff HEAD \| shasum -a 256` = `e5527dd0d8752442e4fec4a785b4f77349e493b732d747c742ed07cf7d005edb`（采集于 2026-09-27 21:36:58） | 工作树随时可能被后续写者改动<br>**口径修正**：该指纹是**补验前时点值** —— 采集**在 U9/U5 补验与本次回填之前**；本次回填编辑会使未提交计数 **+1**（12 → 13，即本记录自身），故复现时不得以「12 项」为唯一判据 | 复现本记录时若指纹不符，说明树已漂移<br>**残余**：§0 的初版快照（97 项 / `32903b47…`）不再代表当前树；U9/U5 并行补验期间树可能再次漂移 | 以 §0 的 `git status` 计数 + 树指纹为准，漂移时重取<br>**重取命令原文（本轮已执行）**：`git status --porcelain \| wc -l`；`git diff HEAD \| shasum -a 256`（结果见 §0「树指纹重取（补验前时点）」行） |

**与 §4 的关系**：§4 每条 S 变更末尾的「残余 UNVERIFIED」均已收入本节（S1→U1、S4→U3、S5→U4、S6 为已实测事实无残余、S7→U2）；§5 的 1:N 与 `None` → U5/U6。

---

## §8 交付门禁核对表（plan §7 的 9 项）

| # | 门禁项（plan §7 原文） | 判定 | 证据指针 | 备注 |
| --- | --- | --- | --- | --- |
| 1 | `workspace` 进 `BUILTIN_MCP_INSTANCES`，`find("workspace")` 命中，overlay 自动注入 5 个实例 | ✅ 实测 | `peri-acp-types/src/builtin_mcp.rs`（`WORKSPACE_TOOLS` 7 项 + 第 5 个实例条目；`BUILTIN_INSTANCE_POLICY_KEYS` 现为 `WebMiddleware/ArtifactMiddleware/CronMiddleware/LspMiddleware/WorkspaceMiddleware`，数组本体已逐字核对）；§3.6 01 `builtin_mcp::tests` 11 绿（含 `find_hits_only_implemented_instances`）；§3.6 03 `overlay_injects_every_reserved_instance`、`injection_policy_all_and_none_shapes`（期望序 `web/artifact/cron/lsp/workspace`） | `system_mcp`/`system_mcp_tools` 由 `builtin_default_entry` 派生，**未手写**（AW3-03） |
| 2 | 首个 LLM 请求直连表含 7 个 `mcp__workspace__*`、不含 7 个裸名 | ✅ 实测 | print：§3.2 的 5 个工具表请求（`req-1..5`）全部 `total=17 / mcp=10 / ws=7 / bare=[]`；TUI：§3.5 面板 `tools: 7` + 同一断言口径 | 负对照 §3.3（`--bare` ⇒ `total=7 / ws=0 / bare=0`）证明该判据**可区分** |
| 3 | 搜索面逐工具 XOR（裸名恰不命中、effective name 恰命中） | ✅ 实测 | §3.4：条目级 CSV XOR PASS；x1/x2 查询命中集 `entry_names` 无精确同名项；x3 `mcp__workspace__Read` 真实执行出文件内容；x4 裸名 `Read` ⇒ `Tool not found: Read` | 覆盖「搜索列出」与「搜索后执行」两端 |
| 4 | N1–N8 全部闭合且有反例实验证据 | ✅ 实测（且超范围闭合 N9/N10） | §6.2 表（N1–N10 逐点落点 + 具名用例 + 破坏必红原文 + 还原 sha）；终态绿见 §3.6 批次 | 差集：plan §3.2 的 8 点已全含；N9/N10 为计划外侦察发现（plan 已增补 C5） |
| 5 | `MIDDLEWARE_NAMES` 无 `FilesystemMiddleware`/`TerminalMiddleware`；`BUILTIN_INSTANCE_POLICY_KEYS` 含 `WorkspaceMiddleware`；`MIDDLEWARE_TOOL_NAMES` 无 7 裸名 | ✅ 实测 + 静态逐字核对 | 三个数组**本体**已逐个取出核对：`MIDDLEWARE_NAMES`（22 项，无二者）、`BUILTIN_INSTANCE_POLICY_KEYS`（5 项，含 `WorkspaceMiddleware`）、`MIDDLEWARE_TOOL_NAMES` 内 7 裸名匹配计数 **0**；§3.6 02 `meta_harness::tests` 全绿（含 `middleware_names_and_builtin_policy_keys_are_disjoint`、`builtin_instance_policy_keys_match_declaration_table`） | T10 亦已完成：`middleware/filesystem.rs` 已删、`TerminalMiddleware` 类型已无（`terminal.rs` 保留 `BashTool` 实现，AW3-02） |
| 6 | 全量门禁四段绿 | ✅ 实测 | §3.1：`build --workspace` / 16 个 lib target（6609 passed / 0 failed / 15 ignored）/ doc tests（11 passed / 5 ignored）/ `clippy -D warnings` / `fmt --check` / 层门 18 规则违规边 0 —— 全部 exit 0 | 段时耗被 cargo 指纹命中缩短，已在 §3.1 自我披露并指向 `/tmp/d3_gate/` 的全时耗运行。**终态复跑见 §3.10 ③**：除 `cargo test --workspace --lib` 的唯一红 target `peri-tui` 两例（既有隔离缺陷 W4-11，串行 `1684 passed / 0 failed` 绿）外全绿 |
| 7 | D2 真实二进制 print + TUI 双通道证据 | ✅ 实测 | print：§3.2（`/tmp/w3_smoke3/W3/`，5 工具表请求 + 搜索 + 负对照）；TUI：§3.5（`/tmp/w3_tui_smoke/w3tui/`，含 MCP 面板 `ready 5/5`、`workspace ✔ connected transport: builtin tools: 7`、turn 完成标记） | 两条通道各自独立采集，非同一进程复用 |
| 8 | 验收记录完成，S1–S5 与 UNVERIFIED 清单齐备 | ✅ 本记录 | S1–S**7**（超集，§4）+ §7 的 U1–U11 + §5 的 1:N/`None` 登记 | 计划写 S1–S5；实际登记 7 条（多出 S6/S7 系 plan 增补） |
| 9 | 文档面（C4）同步完成 | ✅ 文件级实测（内容正确性见 U10） | `git diff --stat -- docs example scripts`：13 文件 / +128 −92 —— `docs/code-index/{peri-acp-types,peri-acp,peri-agent,peri-middlewares,peri-tui}.md`、`docs/design/{meta-harness,middleware-system}.md`、`docs/meta-harness.md`、`docs/reference/mcp-ecosystem.md`、`docs/standards/architecture-contracts.md`、`example/minimal/{.peri/settings.json,README.md}`、`scripts/import-exemptions.conf` | `docs/design/mcp-adaptation-v4-part-1.md` **按 AW3-10 不回填**（目标设计冻结） |
| 10 | 本轮遗留补验（U1–U11）完成情况 | ✅ 11 条已补验完成（其中 U5 判定反转、U8 部分反转、U6-3 结构性不可达） | §7 表逐行更新（U1–U11 全行）+ §0 指纹重取 + §3.8（U6/U7 新增 6 例）+ §4 补验指针 + §5.3 证据列更新 + §6 判定准则同步 | U6 部分闭合（U6-1/U6-2 闭合；**U6-3 结构性不可达**，留 wave 4（W4-5））；U8 判定部分反转；**U5 判定反转**（1:N 形态无第二次注入尝试，真退化为静默冒充）；**U9 三组终态重放**（N1/N2、N8、N3；其余组列为残余）；候选议题见 §9 |

| 11 | 变更区域清理与交付终态（9 提交 / 终态门禁 / 生产二进制验收 / 破坏必红 / 证据更正） | ✅ 已完成（唯一 FAIL 为既有隔离缺陷，已定性并登记） | §3.10：① 身份、② 批次、③ 门禁（含 23:46–23:50 复核重跑）、④ 二进制验收、⑤ flake 定性、⑥ 破坏必红、⑦ 证据更正登记；逐提交钩子日志 `/tmp/v4p3_commit_verify/commit{1..9}.log` | 交付 HEAD `1547b840`；工作树在 23:33 采集时干净（23:48 起并行 campaign 另有未提交改动，见 ③ 补记时点口径）；`cargo test --workspace --lib` 唯一红 target 为 `peri_tui` 两例（既有缺陷，登记 W4-11；复跑可全绿）；记录层 10 条失配见 ⑦ |

### §8.1 与冻结形态/计划的偏差登记（均已在正文各节给出理由）

| # | 偏差 | 依据 |
| --- | --- | --- |
| 1 | AW3-11 的「两名成员槽」落成**单槽** `workspace_input: Option<WorkspaceInstanceInput>`（两成员在该结构体内） | §5.1；plan §2 AW3-11 表项 1 只约束「增设 session 级输入槽」，未约束槽的嵌套层数 |
| 2 | `on_bg_complete` 的构造 helper 落在 `peri-agent/src/session/bg_complete.rs`（plan §2.1 要求「优先在 `peri-agent` 侧提供小 helper」） | §5.1；层门已过（§3.1） |
| 3 | `make_task_manager` 合并进 `new_session_task_manager`（原私有方法已无第二调用者） | §5.1；语义仍是唯一工厂调用点 |
| 4 | `SessionEnvironment.workspace_input` 字段/访问器收窄为 `#[cfg(test)]` | 生产无读者（读取面在 `HostAssemblyInput`），未用私有字段/`pub(crate)` 访问器在 `clippy -D warnings` 下会因 `dead_code` 报错，而本波纪律不新增 `#[allow]`；§3.6 15 的 `ptr_eq` 断言依赖「两落点各自持值」（同源派生会让断言退化为自比较） |
| 5 | 层门豁免新增 `host/workspace.rs` 进 `ACP-biz-fullpath` | 该文件仅取 `peri_middlewares::assembly::WorkspaceInstanceInput`（公开 re-export，A33 禁的是 `mcp::builtin` 内部面）；`scripts/import-exemptions.conf` 同批更新；层门 exit 0 |
| 6 | 计划外新增 N9/N10 与 C5 任务 | 侦察发现（plan 已增补 §3.2 行）；§6.2 已给反例证据 |
| 7 | N1/N2 共用 `subagent/mod.rs` 的 helper（未各复制一份）；N3 helper 为 `pub(crate)`；`name_candidates` 未提升 `pub` | §6.4 逐条理由 |
| 8 | AW3-08 纪律的执行记录：W3-A 提交只枚举 `mcp::builtin` 过滤集、漏记 `mutation_tool_matches_original_name_policy_for_builtin_names`（`declared == 7` 随注册表扩到 14 而红） | plan AW3-08 反面案例；本波已把该计数断言改为注册表派生并在 §3.6 05 覆盖（§6.2 N2 行） |
| 9 | **计划原文与代码事实不符**（非实现偏差）：AW3-06 称摘除的旧键「会被拒收」，实际是 warn + 忽略 | §4 的「S3 附注」（引用 `peri-acp/src/provider/config.rs:391`、`:415-419`）；文档已按事实更正 |

### §8.2 本记录未覆盖的交付（留给后续）

- **提交动作**：本波全部改动（W3-A 之后的 B/C/D 三段）与本记录**同批提交**；commit message 已按 AW3-08 写明「当前红灯集合与归属」——**提交前终态四门全绿（§3.7）⇒ 红灯集合为空集**，此句即记录。哈希见 `git log -1`（记录与提交同批，无法自引哈希；本记录为未跟踪新增文件，其自身变更不计入 `git diff HEAD` 指纹 `32903b47…`）。**（补验：U11）** 提交 `1411ed39` 后本记录**已被跟踪**（`git ls-files` 命中、且不在 `git status --porcelain` 输出中）⇒ 本次回填编辑**会**计入未提交计数（12 → 13 项）；补验前时点指纹另见 §0「树指纹重取」行。
- **1:1 fail-fast 断言**（§5.2）与 **capability root**（U3）为 wave 4 议题。
- **`docs/design/mcp-adaptation-v4-part-1.md` 不回填**（AW3-10 冻结）。

---

## §9 wave 4 backlog（候选）

2026-09-28 合并安全复验：W4-3 / W4-4 已闭合，§7 U2 保留为修复前历史观测，不再代表当前行为。当前 IF-D14 允许类型化安全原因及工具生成的任务/日志/草稿引用；仅 workspace builtin 使用原生期限，其他 builtin 与外部 MCP 共用 120 秒发送/响应期限。回归见 `peri-middlewares/src/mcp/builtin/workspace_recovery_test.rs`（真实 120 秒、live Read、取消后进程组退出、TaskManager Complete）及 `peri-tui/tests/print_background_exit.rs`。未知错误仍脱敏；blocking 搜索只协作取消，外部服务是否遵循 MCP 取消由服务实现决定。


> **本节是候选清单，不是本波交付物**：W4-1…W4-10 逐条抄录自本轮补验台账（`.peri/plans/w3-u-ledger.md` 末尾的同名表，编号即该表编号）；**W4-11 为 §3.10 终态门禁的新发现**，不来自该台账；是否纳入 wave 4 由后续计划裁决。

| # | 议题 | 来源 |
| --- | --- | --- |
| W4-1 | capability root（文件工具越界 / Bash 无沙箱）—— 基线已补（U3） | AW3-04 / U3 |
| W4-2 | 1:1 fail-fast 断言（`session_resources=true` 形态若被当 root 应 fail-fast，而非静默首胜） | §5.2 / U5 |
| W4-3 | **已闭合（2026-09-28）**：类型化恢复回执保留 task_id / PID / live 日志和处置指引；不透传任意错误或输出 | U2 修复前观测；`workspace_recovery_test.rs` |
| W4-4 | **已闭合（2026-09-28）**：workspace builtin 原生期限消除桥竞争；桥取消通知送达 handler，Bash 由会话 owner 清理并验证进程退出 | U2 修复前观测；`workspace_recovery_test.rs` |
| W4-5 | 主链「挂起→唤醒→空转」时序（需 agent-loop 级夹具；生产不出现） | U6-3 |
| W4-6 | `name == "Agent"/"Task"` 归一脆弱点（将来迁移会静默失效） | U1 相邻风险 |
| W4-7 | `namespace()` 转发（声明段分组排序退化；文本不受影响）（回填注：本轮 U8 实测**顺序不变、索引 diff = 0**；「文本不受影响」仅指 `{{namespace}}` 占位符计数 7/7 = 0，模板正文的裸名经 `{{name}}` 渲染后确有变化且属设计必需） | U8 |
| W4-8 | `connection_summary` 竞态（同请求 `tools[]` 与摘要不一致） | U8 顺带发现 |
| W4-9 | 文档残余：`docs/design/tool-system.md:90`、`docs/design/meta-harness.md:261`、`docs/peri-internal-architecture.html:3156` | U10 |
| W4-10 | `peri-acp` 宿主侧取消路径端到端探针 | U7 残余 |
| W4-11 | **既有测试隔离缺陷**：`peri-tui/src/acp_client/client_reverse_test.rs` 12 个 `#[tokio::test]` 未加 `#[serial]` 却改写进程级全局 atoms，使 `kit::acp_bridge::tests` 两例在并行全量下 ~50% 假红（串行必绿）。修法候选：补 `#[serial]` 或改用不共享全局的夹具 | §3.10 ⑤（终态门禁） |
| W4-12 | `peri-middlewares/src/permission/mod.rs:532` 的批量审批超时文案写死 `BROKER_TIMEOUT`(300s) 而非 `self.broker_timeout`（单条路径 `:420` 同病，既有）；用例只断言 `contains("超时")`，50ms 配置被报成 300 秒也测不出 | §3.10 ⑦ 行 9 |
| W4-13 | `peri-agent/src/middleware/chain.rs:175` 的 `break` 与既有空守卫（`:151-153`）行为冗余（防御性代码）；且新增用例把实现 `format!` 文案逐字复制进断言（源耦合，措辞一改双向红） | §3.10 ⑦ 行 5 |
