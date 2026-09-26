# MCP adaptation v4-part-2 — 主实施计划（wave 1：Builtin MCP 运行时 + Web/Artifact MCP）

> 日期：2026-09-26。状态：**v2 已吸收裁决**（IF-D1…IF-D12 逐条校验见 §0.1；两份独立对抗性核查的 12 条 BLOCKER 与 20 余条 MAJOR 已由负责人裁决为 A1–A19，落点见 §0；裁决处一律以本文件为准，冲突旧表述已删除）。代码未实施。
>
> 目标事实源：`docs/design/mcp-adaptation-v4-part-1.md`（下称「设计文档」，契约语义唯一事实源）。批次编排与接口冻结的事实源是本文件。上一批次：`spec/issues/2026-09-25-mcp-adaptation-v4-part-1-plan.md`（下称「part-1 plan」），本文件沿用其文档惯例（§3 冻结接口 / §4 文件所有权 / §5 覆盖登记 / §6 任务表 / §8 验收矩阵 / §9 施工规则 / §12 sub-plan 索引）。
>
> 现场证据另存：`spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（**新建**，不得追加到 `2026-09-25-mcp-adaptation-v4-part-1-acceptance.md`；owner = V-06（W0：迁移前基线小节）→ V-04（W5：终态与三态列））。本文件不保存某一次执行的勾选状态、耗时与提交号。
>
> 三态口径沿用 part-1 与 acceptance：「目标归属」（设计文档）≠「当前实现」（代码事实）≠「本次运行时证据」（命令 + exit + 测试数）。绿色局部单测不得升级为整体迁移结论。

## 0. 裁决记录 A1–A19 落点（复核用索引）

> 口径：本节的「落点」是本文件内**唯一存续**的表述位置；与之冲突的旧表述已在 v2 中删除或改写。子计划内的对应小节由各子计划头部的同名 §0 登记。实施者遇到任何冲突，以本文件 §3（冻结接口）与 §5（覆盖登记）为准。

| 裁决 | 内容摘要 | 本文件落点 |
| --- | --- | --- |
| **A1** 注入点唯一 | builtin 默认层注入点 = `mcp/config.rs` loader **step 6.5**（单一配置事实源） | §0.1 IF-D3 行；§3 IF-D3「注入点」（冻结）；§5 R2（重写）；§10 R9/R11；§12 sub-plan E 覆盖清单 |
| **A2** 回退开关语义 | `PERI_MCP_BUILTIN`：off = **没有** Web/Artifact 能力（middleware 已删，无旧实现回退路径），是显式运维开关；与 `disabled: true` 的区别 | §3 IF-D3「策略来源」+ IF-D9「紧急闸门」；§8 第 8 行；§10 R5；§9 规则 9；acceptance 要求见 §6 V-04 行 |
| **A3** 保留实例名 | `web`/`artifact` 保留 + 预留 `cron`/`lsp`/`workspace`；用户为保留名声明 `command`/`url` → 加载期 typed error；`disabled: true` 仍合法；反例测试 | §3 IF-D3 规则 3 + IF-D4「保留名」；§5 R20；§8 第 9 行；§9 规则 10 |
| **A4** 生效名归一原则 | 单一 helper 落 `peri-acp-types`；7 个消费点全集冻结；判定型=替换、匹配型=原样优先；事件载荷/wire 仍为 effective name | §3 IF-D6（重写）+ IF-D7（重写）+ IF-D15（新增冻结接口）；§4 文件矩阵（TUI / hooks / tool_catalog）；§5 R21；§8 第 10 行 |
| **A5** 直连性声明 | 声明表逐工具 `direct`；在 `build_typed_tool_bridges` 生效；未类型化版本行为不变 | §3 IF-D13（新增冻结接口）；§4（`tool_bridge.rs` → E-03）；§5 R22；§8「投影同步」行 |
| **A6** 三个工具面等价 | 主链 / 子 agent 继承 / workflow agent 三面都要有可观察能力面断言 | §2 事实行（`preparation.rs:123-140`、`workflow.rs:87/:151`）；§3 IF-D10 第 2 条 + IF-D8；§5 R9（重写）+ R23；§8 第 11 行；§10 R4（重写） |
| **A7** 槽位与名单 | 删两 `ChainSlot`；`MIDDLEWARE_NAMES` 仅链槽位名；新增 `BUILTIN_INSTANCE_POLICY_KEYS`；断言改「强度不降」形态 | §3 IF-D7（表 A）+ IF-D8；§5 R15（重写）+ R24；§4（`provider/config.rs` → S-02）；§11「`MIDDLEWARE_NAMES` 的语义」行 |
| **A8** TUI 在范围内 | 用 A4 归一（不得硬编码 `mcp__web__*`）；S-08 落 4 文件 + 自带测试 | §4 文件矩阵（`peri-tui/**` → S-08）；§6 S-08 行；§7 W3；§5 R25；§10 R4 |
| **A9** 声明段不得丢失 | 迁移后仍产出等价声明；机制 = 声明表携带模板 + builtin direct 桥实现 `prompt_declaration()` | §4（`tool_search/declaration.rs`+`declaration_test.rs` → S-02）；§5 R26；§6 S-02 行；§8 第 12 行 |
| **A10** V-03 可达性 | 新建 host 侧 wire 夹具；`PtcScriptedModel` 私有 ⇒ 需复刻 | §4（`mcp_v4_wire_fixture.rs` → V-02）；§5 R27；§6 V-06/V-02 行；§8 第 4 行 |
| **A11** 隔离夹具 env | 夹具设 `PERI_MCP_BUILTIN=off`（只改夹具 env，既有断言不改）+ 新增 off 断言；登记为允许改动 | §4（`tests/mcp_isolation_contract.rs` 行）；§5 R28；§7 W4 闸门；§8 第 13 行 |
| **A12** 基线证据 | W0 在迁移前 HEAD 录「首个 LLM 请求工具名」基线入 acceptance | §6 **V-06**（W0）行；§7 W0 闸门；§8 第 14 行；acceptance 要求 §6 V-04 行 |
| **A13** 隔离断言可观察化 | 四项可观察断言替代不可证伪项；capability root / 凭据降级 UNVERIFIED | §8「契约 5 扩展」行（重写）+ 第 15 行；§10 R8；§9 规则 9 |
| **A14** 任务号与过滤器 | 子计划的旧装配任务编号统一为 **`I-03`**（装配/链/槽位只有一个 task 号）；精确过滤器；`session::factory` 无测试模块 ⇒ 断言改挂；新模块必须配对测试与挂载 | §4（全表 owner 列 + 测试挂载列）；§6（全部「验证命令」列）；§9 规则 3/4/11；§3 IF-D4「被它约束的测试」 |
| **A15** `call_tool` 映射冻结 | 新增 IF-D14（owner I-01），按 spike 形状冻结成功/失败两种形态 | §3 IF-D14（新增冻结接口）；§5 R29；§6 I-01 行 |
| **A16** duplex 容量语义 | capacity 只影响背压，不是帧上限；加大 payload 用例 | §3 IF-D1「duplex」注；§6 E-03 行；§8 第 16 行；§12 E 覆盖清单 |
| **A17** `{"web": {}}` 语义 | 规则 2 在 `disabled != Some(true)` 时同时填 `system_mcp` + `system_mcp_tools` | §3 IF-D3 规则 2 + IF-D9；§5 R14（重写）+ R30；§8「默认层与覆盖语义」行 |
| **A18** 关闭片段形状 | 冻结唯一合法片段（只写 `disabled: true`）；非法组合必须被拒绝且不致命 | §3 IF-D3 规则 6；§5 R31；§8 第 17 行；§9 规则 10 |
| **A19** 主计划唯一裁决源 | 吸收子计划 G 的未登记任务；`sensitive_tool_entries` 条目名改 effective name；docs owner 与 doc 检查项；acceptance 新建与模板 | §4（S-03/S-04/S-05/S-06/S-08/V-05 行）；§5 R32/R33；§6（新增 S-03/S-04/S-05/S-06/S-08 行、V-05 行扩展）；§8 第 18 行；§9 规则 12 |

## 0.1 设计决策校验记录（IF-D1…IF-D12）

校验口径：每条以 `文件:符号` 的代码事实为准；「成立」= 可直接实施；「需修正」= 前提正确但方案不完备，本文件给出替代；「必须新增」= 原清单遗漏且遗漏会直接违反既有契约。

| 接口 | 裁决 | 已核实依据 | 修正 / 补充 |
| --- | --- | --- | --- |
| **IF-D1** 新增 `TransportConfig::Builtin` | **成立（需补 1 项）** | `transport.rs:10-22` 只有 `Stdio`/`StreamableHttp`；`TryFrom<&McpServerConfig>` `:35-55`；穷尽 `match` 落点 `initialize.rs:275`（两臂 `:276`/`:299`）、`reconnect.rs:90`（两臂 `:91`/`:120`）；二元 `is_http` 判定 `initialize.rs:262`、`reconnect.rs:76`；`serve_client_auto`（`client/transport.rs:18-57`）与枚举无耦合，spike 已用 `tokio::io::duplex` + `(ReadHalf, WriteHalf)` 元组走通 | 补：连接超时选择必须从「http / 否则 stdio」的二元改为**三分类**。否则 builtin 会复用 `STDIO_CONNECT_TIMEOUT`（`client.rs:119`）并把 `is_http` 变成事实错误（`initialize.rs:443` 的 `transport = if is_http {"http"} else {"stdio"}` 日志字段同源） |
| **IF-D2** 新增 `ConfigSource::Builtin` | **成立（需载荷）** | 变体现为 `plugin.rs:24-31`；唯一穷尽 `match` 在 `discover_tool.rs:331-335`；`source` 是 `#[serde(skip)]`（`plugin.rs:108-110`）→ 用户配置无法伪造 | 修正：冻结为 `ConfigSource::Builtin { instance: String }`。理由：`TryFrom<&McpServerConfig>` 拿不到 server name；不带载荷就必须改签名为 `for_server(name, config)`，波及 3 个调用点（`initialize.rs:253`、`reconnect.rs:70`）与既有测试。载荷式变体让 `TryFrom` 保持唯一入口与既有签名 |
| **IF-D3** builtin 默认层优先级最低 | **成立（注入点与覆盖规则已冻结；已对 sub-plan F 仲裁）** | 合并算法 `config.rs:324-436`（step 1–7）；loader 消费方 `initialize.rs:127`（生产）、`:516`（`#[cfg(test)]` helper）、`config.rs:443`（公开 `load_merged_config`）；admin 循环 `initialize.rs:173-227`；空集合短路 `:178-185`；`system_requirements()` 只认 `system_mcp == Some(true)`（`readiness.rs:349-363`）；配置层禁用即 `Err(Disabled)` fatal（`readiness.rs:466-479`） | 注入点冻结为 **loader 的 step 6.5**（step 6 变量展开循环 `:419-430` 之后、step 7 `validate_config(&merged)?` `:433` 之前）：单一「有效配置」事实源，`load_merged_config`（`config.rs:442`）与运行时不再出现两套配置（**采纳 sub-plan F IF-F1 的位置论证**；**否决**「注入点为 `initialize_config`（`initialize.rs:146`）函数体顶部」的旧表述）。覆盖规则（冻结）见 §3 IF-D3：① 名字缺失→插入完整 builtin 条目；② 名字存在且 `command`/`url` 均为 `None`→填 `source`，并在 `disabled != Some(true)` 时**同时**填 `system_mcp = Some(true)` 与 `system_mcp_tools`（A17；`disabled == Some(true)` 时只填 `source`，保持 `Disabled` 注册语义），其余字段以用户值为准，不做字段级深合并；③ 名字存在且声明了 `command` 或 `url` 且该名字是**保留实例名**→加载期 typed error（A3；不再是「整条不动/用户接管传输」）；④ 唯一合法关闭片段 = `{"web": {"disabled": true}}`，`{"web": {"disabled": true, "system_mcp": true}}` 必须被加载期拒绝（A18，避免落到 `readiness.rs:466-479` 的 `Err(Disabled)` fatal）。**否决 sub-plan F 的新字段方案**（IF-F2 的 `McpServerConfig.builtin`）：不新增配置 key、不做 struct literal 收口，身份由 `source` 承载（`#[serde(skip)]` 已保证用户无法伪造）。硬约束 ① 策略是显式参数，禁止在纯函数内读 env（生产侧只在 `load_merged_config_full` 读一次）② 「用户 `disabled: true` + 内置 `system_mcp: true`」不得合成 disabled 的 system 依赖（规则 ② 已保证：`disabled == Some(true)` 时用户条目的 `system_mcp` 不被填充）
| **IF-D4** `peri-acp-types` 纯数据声明表 | **成立（需 1 条禁止 + 2 项载荷）** | `sanitize_name_component` 是 `pub(crate)`（`tool_bridge.rs:56-66`）；`effective_mcp_tool_name` `:95-101`；`McpToolBridge::original_tool_name()` 已存在（`:200`，注释明确「effective name 的净化不可逆…不得反拆 `name()`」）；`peri-acp-types` 无法依赖 `peri-middlewares` | 禁止在 `peri-acp-types` 复刻 sanitize 规则（那等于同一规则两份）。冻结：原始名清单 / 策略键 / **逐工具的 `direct`（A5）** / **逐工具的 `prompt_declaration` 模板（A9）** / **冻结的 effective name 字面量（供归一 helper 查表）** 等**纯数据**放 `peri-acp-types`，effective name 的计算与实例解析全部落在 `peri-middlewares/src/mcp/builtin/`；两处的对齐由 E-02 的字面量锁死测试保证（规则只有一份实现，字面量只有一份声明） |
| **IF-D5** 模型面冻结名 | **成立（已核算；并作为 A4/A5/A9 的载荷）** | `sanitize_name_component` 对 `[A-Za-z0-9_-]` 恒等（`:57-65`），effective name 模板 `mcp__{sanitize(server)}__{sanitize(tool)}`（`:96-100`）；三个工具今天均为 direct（`web_fetch.rs:95`、`web_search.rs:76`、`artifact/tool.rs:109`）；三者的 `prompt_declaration()` 实现于 `web_fetch.rs:106-111`、`web_search.rs:87-92`、`artifact/tool.rs:121-127` | `mcp__web__WebSearch`、`mcp__web__WebFetch`、`mcp__artifact__artifact` 三点均为实际函数输出形态（不改大小写、不换下划线）。必须由字面量测试锁死（E-02）；同一张表逐工具携带 `direct`（A5）与 `prompt_declaration` 模板（A9），并由「声明表字面量 == `effective_tool_name()` 输出」与「`direct` 集合 == `system_mcp_tools` 集合」两条断言兜底 |
| **IF-D6** 策略一致性层 | **成立（形状已按 A4 改写）** | `permission/mod.rs:44-59`（`mcp__` 前缀无条件 true）、`:65-67 is_edit_tool`、`:91-164 sensitive_tool_entries()`（14 项，3 条前缀：`delete_`/`rm_`/`mcp__`）；锁定测试 `permission/mod_test.rs:437-467`、`:472-482`；`subagent/mod.rs:389-395 is_mutation_tool`、`subagent/mod_test.rs:497-503`；`peri-acp/src/prompt/prompt_test.rs:227-247` 断言渲染出的 `10_hitl` 段落内容来自 `format_sensitive_tools()` | 改为 **A4 的单一归一机制**（不是「逐点特判」）：helper `original_tool_name_of_effective()` 落 `peri-acp-types`（IF-D15），**判定型**消费点（①②③）= 命中则对原始名判定（替换语义，不得改写成并集）；**匹配型**消费点（④⑤⑥⑦）= 原样优先、未命中再用原始名。补：① `mcp__` 前缀条目的 description 必须改写（迁移后 `mcp__artifact__artifact` 不再敏感，「any MCP server tool」条文失真）；条目数与前缀数不变（14 / 3）② `sensitive_tool_entries()` 中已迁移条目（`WebFetch`/`WebSearch`）的 `name` 改为 effective name `mcp__web__WebFetch` / `mcp__web__WebSearch`，一致性测试探测名同步（A19/C11）③ `is_edit_tool` 同样走归一（parity 后仍为 false，但要写死语义）④ parity **不得**改变未知 `mcp__*` 的保守 true，否则 `mod_test.rs:437-450` 的 `mcp__some_tool` 探针会红 ⑤ **事件载荷与 wire 仍暴露 effective name**，归一不得泄漏到投影真值 |
| **IF-D7** 投影与静态表同步 | **成立（形状已按 A7/A4 改写）** | `meta_harness.rs:163-201 MIDDLEWARE_TOOL_NAMES`、`:93-121 MIDDLEWARE_NAMES`（现 27 项，含 `WebMiddleware`/`ArtifactMiddleware`）；锁定测试 `assembly_test.rs:1207-1217 middleware_names_match_production_blueprint`（集合相等）、`:1220-1249 slot_middleware_name`（穷尽 match）；剔除谓词 `stage_builder/tools.rs:22-45`（按 `MIDDLEWARE_TOOL_NAMES` 名字剔除）；`peri-acp/src/provider/config.rs:396-426 validate_meta_harness`（known 集合 `:400-405`、`all_middleware_disabled` `:419-426`）；**新增发现**：`peri-acp/src/event/tool_projection.rs:61-70 infer_tool_kind` 有 `"WebFetch" \| "WebSearch" => ToolKind::Fetch`；`peri-agent/src/tools/invocation.rs:120-125 TOOL_PARAM_ALIASES` 有 `("WebSearch","search_term","query")`，按 `target.name()` 精确匹配（`:167-171`）；TUI 按名分支 `kit/tool_display.rs:53,58`、`truncate.rs:186,190,275`；`hooks/matcher.rs:10`/`:32` 按名匹配；`session/tool_catalog.rs:119 ToolFilterPolicy::canonical` 按名过滤 | ① `MIDDLEWARE_TOOL_NAMES` 删除三个裸名 ② **两张表**：`MIDDLEWARE_NAMES` 只保留**链槽位名**（删 `WebMiddleware`/`ArtifactMiddleware`，27→25），新增 `BUILTIN_INSTANCE_POLICY_KEYS`（这两个策略键）③ `assembly_test.rs` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == 声明表 `policy_key` 集合」∧「两者交集为空」（等价于「并集 == 两表并集」，强度不降），`slot_middleware_name` 删两臂 ④ `provider/config.rs` 的 known 集合与 `all_middleware_disabled` 改为两表并集 ⑤ 所有按名消费点（`infer_tool_kind` / `TOOL_PARAM_ALIASES` / TUI / hooks / `canonical`）改走 A4 的归一 helper（**禁止**逐点硬编码 `mcp__web__*`）⑥ `stage_builder/tools.rs` 谓词注释按「builtin 工具以 `mcp__*` 形式存在于 MCP 目录，从不进入 `shared_tools`」重新论证 |
| **IF-D8** 槽位处置（owner：I-03） | **成立** | `peri-agent/src/session/factory.rs:112`（`ChainSlot::Web`）、`:127`（`Artifact`）、`:56-57`/`:82-83`（枚举定义）；挂载点 `assembly.rs:272-275`、`:361-364`；`preparation.rs:131-133`；`workflow.rs:104-106`、`:205-207`；`assembly_test.rs:453`/`:464 slot_name`、`:1220-1249 slot_middleware_name`、`:394 blueprint_sequence_is_canonical`、`:505 artifact_middleware_can_be_disabled_independently`、`:1207-1217 middleware_names_match_production_blueprint` | 冻结：删除两个 `ChainSlot` 与 blueprint 两项，同步删除 4 个挂载点的 web/artifact 分支；`MIDDLEWARE_NAMES` 同时删两键并新增 `BUILTIN_INSTANCE_POLICY_KEYS`（A7/C10）；`slot_middleware_name` 对应两臂删除；**`peri-agent/src/session/factory.rs` 无任何测试模块**，因此该文件的相关断言改挂 `cargo test -p peri-middlewares --lib -- assembly::tests` 与 `cargo test -p peri-agent --lib -- session::exec::stage_builder`（A14）。工具面变化必须显式冻结（§10 R3/R4/R23），不得静默 |
| **IF-D9** 两实例的 `system_mcp` 声明（owner：I-02） | **成立（需补风险条款 + A2/A5/A17 语义）** | `readiness.rs:349-363` 只认 `system_mcp == Some(true)`；`prepare_system_tools` all-or-nothing（`system_tools.rs:62+`）；`before_react_start` 失败即 fatal（`middleware.rs:703-716`）；`PERI_MCP_APPS` 同层开关 `apps.rs:216` | 补：builtin 进入 system 依赖后，**任何 builtin 连接 / 发现 / 必需工具校验失败都会 fatal 阻塞每个 session 的 react loop**。必须同时交付 ① 紧急关闭闸门 `PERI_MCP_BUILTIN`（**语义按 A2**：off = 不注入任何 builtin 实例，因而退回态**没有** Web/Artifact 能力——middleware 已删，不存在旧实现回退路径，这是显式运维开关、不是静默降级；与 `disabled: true` 的区别见 §3 IF-D3）② fatal 路径专项用例（V-01 / V-02）③ `system_mcp_tools` 集合 == 声明为 direct 的工具集合（A5/A17 的一致性断言） |
| **IF-D10（新增）** builtin 能力关闭 | **必须新增（本批次最大缺口）** | 迁移后 Web/Artifact 的工具源不再是 middleware，而 `meta_harness` 关闭只作用于链装配（`assembly.rs:105` 的 `disabled` 集合）与 `MIDDLEWARE_TOOL_NAMES` 剔除面（`stage_builder/tools.rs:36-43`）。此时 `"WebMiddleware": false` 会退化为「键有效但无效果」，直接违反 ARC-CAPABILITY-CLOSURE-001（`docs/standards/architecture-contracts.md:60-64`） | 见 IF-D10（§3）：注册表带 `policy_key`，关闭集在下游**三处**过滤（direct 注入 / deferred bridge / parent_tools）+ workflow agent 面（A6 后**不再是**「天然关闭」，必须显式过滤），并给 presence/absence 矩阵测试；`MIDDLEWARE_NAMES` 只留槽位名、策略键迁到 `BUILTIN_INSTANCE_POLICY_KEYS`（A7/C10） |
| **IF-D11（新增）** `transport_type` 三分类 | **必须新增** | `status.rs:172/203/224` 用 `url.is_some()` 二元推断 → builtin 会被显示成 `"stdio"`；集成断言 `peri-middlewares/tests/mcp_isolation_contract.rs:382-383` 期望 `"stdio"`（其夹具是 stdio 实例，语义不受改动的威胁，但必须复跑不改断言） | 见 IF-D11：`ConfigSource::Builtin` 存在时返回 `"builtin"`；其余分支行为逐位保持不变 |
| **IF-D12（新增）** reconnect 与 builtin 任务归属 | **成立前提已由 spike 证明** | Q2（`builtin_spike_test.rs:572-616`）：client `close_with_timeout` 后同进程 server task 靠 duplex EOF 收敛为 `Ok(Ok(QuitReason::Closed))`，**无需 abort**；`McpTaskKey`（`task_scope.rs:31-41`）无 builtin 变体 | 冻结：wave 1 **不**新增 `McpTaskKey` 变体；builtin server task 由 pool 侧 map 持有，关闭 / 重连时按 spike `shutdown` 模式（`:299-308`）有界等待后 abort，禁止留 orphan |

## 1. 目的与范围

### 1.1 wave 1（本批次可执行）

把 v4 设计文档中 Web / Artifact 两个目标 MCP 实例**真正落地**，并打通 builtin 运行形态这一公共前提：

| 交付 | 内容 | owner 前缀 |
| --- | --- | --- |
| Builtin MCP 运行时 | `TransportConfig::Builtin`、builtin 注册表、同进程 `rmcp` server 的 spawn / 生命周期 / 关闭 / reconnect、状态与超时分类 | E |
| Web MCP 实例 | `web` 实例承载 `WebSearch` / `WebFetch`，经 `system_mcp_tools` 成为 direct 工具；删除 `WebMiddleware` 提供面 | I |
| Artifact MCP 实例 | `artifact` 实例承载 `artifact` 上传能力；删除 `ArtifactMiddleware` 提供面，**保留其可单独关闭语义**（改为 builtin 实例关闭键，见 IF-D10） | I |
| 策略一致性 | 单一归一 helper（落 `peri-acp-types`）在**全部 7 个按名消费点**统一：判定型（`default_requires_approval` / `is_edit_tool` / `is_mutation_tool`）= 对原始名判定；匹配型（TUI / `TOOL_PARAM_ALIASES` / hooks matcher / `ToolFilterPolicy::canonical`）= 原样优先再用原始名；`10_hitl` 段落条文与条目名同步 | S |
| 投影同步 | `MIDDLEWARE_TOOL_NAMES` / **两张名单 `MIDDLEWARE_NAMES` + `BUILTIN_INSTANCE_POLICY_KEYS`** / `infer_tool_kind` / `TOOL_PARAM_ALIASES` / `build_session_tool_view` 谓词重新论证 / `provider/config.rs` 键集合 / 面板 `transport_type` | S |
| **TUI 按名分支归一（A8）** | `peri-tui/src/kit/tool_display.rs`、`truncate.rs`（+ 各自自带测试）走归一 helper，使 builtin 工具卡片仍有参数摘要 / 专用截断；独立 crate | S |
| **声明段保留（A9）** | 三个工具的 `prompt_declaration` 迁移后仍产出（声明表携带模板 + builtin direct 桥实现），断言落在 `tool_search/declaration_test.rs` | S |
| **文本面同步（A19）** | 内置 agent `web-researcher.md` frontmatter/正文、prompt 段落（`01_intro.md`/`11_subagent.md`）、内置 skill 文本、`docs/reference/mcp-ecosystem.md` | S / V |
| 证据 | crate 内启动路径 + 审批 approve/reject + wire 计数 + 关闭矩阵 + host seam 首个 LLM 请求 tools + **迁移前基线** + 大 payload 用例 | V |

### 1.2 wave 2 / wave 3（本批次**只规划不实施**）

| 波次 | 内容 | 本批次允许的动作 |
| --- | --- | --- |
| wave 2 | Cron MCP、LSP MCP 实例化 | 只在本文件登记为后续对象；不写代码、不改文件 |
| wave 3 | Workspace MCP（`side-projects/local-mcp-server`） | 只登记前置事实（见 §2 末行）；**不**改其 `Cargo.toml`、**不**把它加入根 workspace members、**不**写生产配置引用 |

### 1.3 非目标

- 不新增宿主 CLI 暴露入口。
- 不改 `ToolSearchMiddleware` 的既有 deferral 契约（只验证不回归）。
- 不回填设计文档（`docs/design/mcp-adaptation-v4-part-1.md`）的批次、勾选状态与提交号。
- 不实现 builtin 的 session 中途声明语义（动态 MCP 路径继续拒绝 System key）。
- 不做跨进程 / 跨机器 MCP 隔离，不宣称 `local-mcp-server` 的 workspace root 是安全沙箱。
- 不为 wave 2 / wave 3 的实例预建抽象（例如通用「实例注册中心」）；只做本批次两个实例需要的最小结构。保留名（`cron`/`lsp`/`workspace`）在本批次**只登记、不实现**（A3）。
- **不**给裸名 `WebFetch` / `WebSearch` / `artifact` 做调用别名（`McpToolBridge::aliases()` 保持空）；归一 helper 只影响**判定与匹配**，不产生第二个可调用名字。
- **不**声称凭据隔离 / capability root 隔离已验证（A13：两者在 builtin 形态下不可证伪，按 UNVERIFIED 口径记录）。

## 2. 事实源与实现状态

| 事实 | 位置 |
| --- | --- |
| 契约 1–4、7 已落地（PASS） | `spec/issues/2026-09-25-mcp-adaptation-v4-part-1-acceptance.md` §2 |
| 契约 5 PARTIAL：凭据隔离 / capability root 隔离 **UNVERIFIED**，五实例未落地 | 同上 §3 |
| 契约 6 PARTIAL：**BLOCKED** = 没有一条用例真正**调用**已提升为 direct 的 MCP 工具并同时观察审批与 wire | 同上 §4 第 9 行 |
| builtin spike 已挂载并跑通（6 个用例） | `peri-middlewares/src/mcp/builtin_spike_test.rs`；模块声明 `peri-middlewares/src/mcp/mod.rs` 末 6 行 |
| spike Q1(a)：真实 `ServerHandler` + `discover` 默认实现 → Auto 走 modern（`server/discover`，`V_2026_07_28`），`peer_info()` 为 `Some` | `builtin_spike_test.rs:471-525`；消费点 `initialize.rs:376-392`、`:388-403` |
| spike Q1(b)/(b3)：`discover` 覆写 `-32601` → 回退 legacy 后 `tools/list` 被 `-32602` 拒绝（缺 per-request `_meta`） | `builtin_spike_test.rs:529-564`、`:634-680` |
| spike Q2：client `close_with_timeout` 后 server task 靠 duplex EOF 自然收敛，无需 abort | `builtin_spike_test.rs:572-616`；收尾模式 `:299-308` |
| spike 对照：从不发 `server/discover`、直接 legacy `initialize` 的连接工具发现可用 | `builtin_spike_test.rs:689-691` |
| 连接建立链（builtin 必须复用同一条） | `peri-acp/src/host/assemble.rs:96-121`（pool 构造）、`:451-464`（`spawn_background(McpTaskKey::Initialize)`）→ `initialize.rs:117 run_initialize` → `:127 load_merged_config_full` → `:146 initialize_config` → `:157 validate_config` → `:161 bind_execution_cwd` → `:253 TransportConfig::try_from` → `:275` 分派 → `:345 retain_service`（`client/lifecycle.rs:59`）→ `:357 list_discovered_tools`（`initialize.rs:32`）→ `:388` 构造 `McpClientHandle` → `:405 try_commit_connection`（`client/lifecycle.rs:214`）→ `:413 commit_discovery_success` |
| 隔离载体 | `MccCapabilityProfile` 是 **pool 级**（`apps.rs:229-268`，构造点 `:218`；存于 `client.rs:111`）；凭据 `FileCredentialStore`（`auth_store.rs:34`，key 粒度 = server name）；`McpConnectionKey` `client/types.rs:106-110`；`McpClientPool.clients: HashMap<String, Arc<McpClientHandle>>` `client.rs:73` |
| 无内置实例自动注册机制 | 配置只来自 `load_merged_config_full`（`config.rs:301`）；空集合即 `Ready{total:0}`（`initialize.rs:178-185`）；唯一环境开关 `PERI_MCP_APPS`（`apps.rs:216`）只影响 capability |
| 配置面字段全集 | `peri-acp-types/src/plugin.rs:46-111`（含 `system_mcp` / `system_mcp_tools` / `system_mcp_timeout`，语义 `:77-107`，`validate` `:220-235`，`ConfigSource` `:24-31`） |
| 启动闸门 | `mcp/middleware.rs:703 before_react_start` → `:363 await_system_ready` → `readiness.rs:385 await_system_connections` → `system_tools.rs:62 prepare_system_tools` → `middleware.rs:405 recheck_system_snapshot` → `:440 startup_tool_update` |
| MetaHarness 关闭键的解析与语义 | `peri-acp/src/provider/config.rs:395-424 validate_meta_harness`（未知键 warn+忽略）；`peri-acp/src/session/frozen.rs:172-201 build_meta_harness_state`；`assembly.rs:105` 消费 |
| `web` / `artifact` 现状 | `middleware/web.rs`（只有 `collect_tools`，`build_tools()` 是外部直构入口）、`middleware/web_fetch.rs:92-97` / `web_search.rs:73-78`（`is_direct = true`）；`artifact/mod.rs`（只有 `collect_tools`）、`artifact/tool.rs:105-111`（`is_direct = true`，`namespace = "meta"`）、`artifact/client.rs`（读 `PERI_ARTIFACTS_URL` / `PERI_ARTIFACTS_TOKEN`） |
| 四处挂载点 | `assembly.rs:272-275`、`assembly/preparation.rs:131-132`、`assembly/workflow.rs:104-105`、`assembly/workflow.rs:205-206` |
| MCP bridge 的实际挂载面 | 生产调用点只有两处：`assembly/preparation.rs:136`（`build_parent_tools`，**deferred**）与 `mcp/middleware.rs:487`（`static_tool_bridges` 的 fallback；正常路径走 `:481-489 prepared_static_bridges`）。类型化版本 `build_typed_tool_bridges`（`tool_bridge.rs:412`）是 IF-M2 的冻结接口；未类型化 `build_tool_bridges`（`:430`）的 deferred 默认由锁定测试 `tool_bridge.rs:503-528` 保证（fixture server 名 `workspace`、`source: None`，不经加载器） |
| workflow agent 的工具与链（A6 对象） | `assembly/workflow.rs::build_tools`（`:87`，web 分支 `:104-106`）与 `build_middlewares`（`:151`，web 分支 `:205-207`）**都只有 `WebMiddleware`，没有 `McpMiddleware`**；`WorkflowAgentMiddlewareFactory` 当前是 ZST（`:24`），工厂调用点 `peri-acp/src/host/assemble.rs:510-511`（该处 `mcp_pool_concrete` 在 `:332` 定义、`bare` 时为 `None`） |
| 子 agent 继承面（A6 对象） | `assembly/preparation.rs:123-140 build_parent_tools`：既 `extend(WebMiddleware::build_tools())`（裸名、**direct**，`:131-133`）又 `build_tool_bridges(pool)`（**全 deferred**，`:136`）；子 agent 链**没有 ToolSearch**（唯一实例化点 `assembly.rs:356`）⇒ 迁移后子 agent 会净失去 Web 能力，除非改走类型化构造 |
| 三个工具的今日直连性（A12 基线对象） | `middleware/web_fetch.rs:95-97`、`middleware/web_search.rs:76-78`、`artifact/tool.rs:109-111` 均为 `is_direct() == true`；三者的 `prompt_declaration()` 见 `web_fetch.rs:106-111`、`web_search.rs:87-92`、`artifact/tool.rs:121-127` |
| `McpToolBridge` 的声明能力缺口 | `McpToolBridge` **未实现** `prompt_declaration()`（穿透 trait 默认 `None`），而 `collect_declarations`（`tool_search/declaration.rs:16-40`）只收集 direct 工具的声明；`tool_search/declaration_test.rs:239-242` 直接调用 `WebMiddleware::build_tools()`，删 middleware 后会编译失败 ⇒ 归 S-02 |
| 保留实例名的必要性 | `permission/mod.rs:55`（`mcp__` 前缀无条件敏感）与 `:44-59` 的按名判定都只按**名字**反查；若外部 server 能占用 `web`/`artifact` 名字，会继承「按原始名判定」的结果（静默移除 `mcp__*` 审批门）⇒ A3 的加载期 typed error |
| TUI 按名分支的实测落点 | `peri-tui/src/kit/tool_display.rs:53`/`:58`（WebSearch/WebFetch 参数摘要）、`truncate.rs:186`/`:190`/`:275`（字段提取与折叠）；`kit/tool_semantics.rs`、`kit/acp_types/current_turn.rs` 经核查**无** Web/artifact 按名分支 |
| `side-projects/local-mcp-server` 是独立 workspace 的 binary crate（无 `[lib]`，自带 `[workspace]` 表、`Cargo.lock`），无生产配置引用，且**不在**根 `Cargo.toml` members（根 members 15 项，不含它） | `side-projects/local-mcp-server/Cargo.toml`；根 `Cargo.toml` |

## 3. 冻结接口（Interface Freeze v1，唯一版本）

约定：本节的签名是**行为+形态契约**；实施者不得改语义，只能在 §5 登记的覆盖项内细化。

### IF-D1 `TransportConfig::Builtin` 与三分类超时（owner：E-01 / E-03）

```rust
// peri-middlewares/src/mcp/transport.rs
pub enum TransportConfig {
    Stdio { command: String, args: Vec<String>, env: HashMap<String, String> },
    StreamableHttp { url: String, headers: HashMap<String, String>, oauth: Option<OAuthConfig> },
    Builtin { instance: String },   // instance 必须能在 builtin 注册表中解析
}

impl TryFrom<&McpServerConfig> for TransportConfig { /* 签名不变 */ }
```

- `TryFrom` 的 builtin 判定**唯一来源**是 `config.source == Some(ConfigSource::Builtin { .. })`，且必须在 `config.validate()?` **之后**判定（非法 System 组合不得建立传输）。
- 未解析到的实例名 → 新增 `TransportError::UnknownBuiltinInstance { instance: String }`；错误文本只含实例名，不含任何路径 / env / 凭据。
- 超时分类改三分类：`Stdio → STDIO_CONNECT_TIMEOUT`、`StreamableHttp → HTTP_CONNECT_TIMEOUT`、`Builtin → BUILTIN_CONNECT_TIMEOUT`（新增常量，冻结为 `5s`；同进程握手 + 一次 `tools/list` 的实测上界远小于它）。
- `initialize.rs:443` 与 `reconnect.rs` 中同源的日志字段必须打印同一个分类结果（`"http" | "stdio" | "builtin"`）；**禁止**保留任一 `matches!(…, StreamableHttp { .. })` 形式的二元判定。
- 被它约束的测试：`mcp::transport`（新增 `test_try_from_builtin_source_marker`、`test_builtin_unknown_instance_is_typed_error`、`test_stdin_and_http_branches_unchanged`），以及编译期穷尽性（`initialize.rs:275`、`reconnect.rs:90`）。

### IF-D2 `ConfigSource::Builtin { instance }` 的传播面（owner：E-01）

```rust
// peri-acp-types/src/plugin.rs
pub enum ConfigSource {
    Project(PathBuf),
    Global(PathBuf),
    Plugin,
    Builtin { instance: String },   // 运行时标记：#[serde(skip)] 字段，用户配置无法构造
}
```

传播面（穷尽，均是编译期可见）：
- `mcp/discover_tool.rs:331-335 config_source_str` → 新增臂返回 `"builtin"`。
- `mcp/client/status.rs` 的 `transport_type`（见 IF-D11）。
- `McpClientHandle.source`（既有字段，`client/types.rs` / `initialize.rs:399`）→ 面板与 discover 工具的来源标签。
- 禁止把 `Builtin` 变体写进任何序列化路径；禁止在 `McpServerConfig` 上新增配置 key。

### IF-D3 builtin 默认配置层（owner：I-02 + E-02 的注册表）

- 注入点（冻结，A1 唯一表述）：`peri-middlewares/src/mcp/config.rs::load_merged_config_full_with_paths`，**step 6.5** —— 在 step 6 变量展开循环（`:419-430`）结束之后、step 7 `validate_config(&merged)?`（`:433`）之前执行 overlay。选这里的理由：单一「有效配置」事实源（`run_initialize` 与公开 `load_merged_config`（`config.rs:442`）看到同一份结果），且注入晚于 step 4 的 hash 去重（`:388-407`，builtin 条目不进 `manual_hashes`）。**本批次不存在第二个注入点**：`initialize_config`（`initialize.rs:146`）只消费 loader 结果，不得在函数体内注入、不得读 env。
- 策略来源（冻结）：`BuiltinInjectionPolicy` 为**显式参数**。`load_merged_config_full`（`:301`）读一次 `PERI_MCP_BUILTIN` 并传给 `_with_paths`；`_with_paths` 与 `apply_builtin_overlay` **禁止**读 env。`config_test.rs` 的 12 处 `_with_paths` 调用点由 I-02 统一补参（生产语义测试传 `all()`，断言旧行为的传 `none()`）；若某测试断言「空配置 → 空集合」，改为传 `none()` 而不是改生产注入。
- `PERI_MCP_BUILTIN` 语义（冻结，A2）：默认**开**；值 `off` / `0` → `BuiltinInjectionPolicy::none()`（不注入任何 builtin 实例）；缺失 / 其它值 → `all()`（未知值 warn + `all()`）。**off 不是静默降级**：middleware 提供面已删除，因此 off 的退回态**没有** Web/Artifact 能力（模型面不存在这三个工具），这是显式运维开关，必须写进 acceptance 与用户文档。与 `disabled: true` 的区别：`disabled: true` 仍注入/保留该实例的配置（builtin 传输）但注册为 `ClientStatus::Disabled`，另一个实例与其它 capability 不受影响；`off` 则两个实例都不注入。
- 保留实例名（冻结，A3）：`web` / `artifact` 为**保留实例名**（已实现），并在声明表内一并预留 `cron` / `lsp` / `workspace`（后续波次，本批次不实现、不注入）。用户配置若给**任一保留名**声明 `command` 或 `url` → **加载期 typed error**（新增 `McpConfigError::ReservedBuiltinInstanceName { name }`，错误文本只含实例名，不含路径 / env / 凭据）。理由：parity 与关闭语义都按名字反查，外部同名 server 一旦接管该名字，会继承「按原始名判定」的审批结果，静默移除 `mcp__*` 审批门。
- 覆盖规则（冻结，替代 sub-plan F 的 IF-F1 不变式；A17 已并入）：
  1. 实例名在 `merged.mcp_servers` 中**缺失** → 插入完整 builtin 条目：`command/url/headers/oauth/args/env/subscriptions = None`、`protocol_version = None`（**必须**为 `None`，否则 Auto 不探测 `server/discover`，见 sub-plan E §3 Q1(a)）、`disabled = None`、`system_mcp = Some(true)`、`system_mcp_tools = Some(<该实例**声明为 direct**的工具原始名>)`、`system_mcp_timeout = None`（缺省 30 s）、`source = Some(ConfigSource::Builtin { instance })`。
  2. 实例名**存在**且 `command.is_none() && url.is_none()`：填 `source = Some(ConfigSource::Builtin { instance })`；且 **`disabled != Some(true)` 时同时填 `system_mcp = Some(true)` 与 `system_mcp_tools = Some(<声明为 direct 的工具原始名>)`**（A17：否则 `{"web": {}}` 会从 direct 静默降级为 deferred，与「实例可用」不符）；`disabled == Some(true)` 时**只填** `source`（保持 `Disabled` 注册语义，不构成 system 依赖）。其余字段一律以用户值为准，**不做字段级深合并**。本规则**仅对已实现实例（`web` / `artifact`）生效**；预留未实现名（`cron` / `lsp` / `workspace`）不写 `command`/`url` 时**不注入任何条目**（否则会构造出 `TransportConfig::Builtin` 解析不到的条目）。
  3. 实例名**存在**且声明了 `command` 或 `url`：若是**保留实例名** → **加载期 typed error**（A3）；否则与 builtin 无关（非保留名照旧，overlay 不触碰）。
  4. overlay 结果必须通过 step 7 的 `validate_config`；注册表自身非法（工具清单为空 / 名字含非法字符 / effective name 与冻结字面量不一致）必须在加载期可见失败，不得降级。
  5. **关闭片段形状（冻结，A18）**：唯一合法的用户关闭写法是 `{"web": {"disabled": true}}`（只写 `disabled`，不带 `system_mcp`）。`{"web": {"disabled": true, "system_mcp": true}}` 这类「禁用 + 声明为 system」的组合必须被**加载期拒绝**（typed error），因为它在今天会走到 `readiness.rs:466-479` 的 `Err(SystemReadinessError::Disabled)` fatal，阻断**所有** session。必须有反例用例覆盖该组合。
  6. **direct 一致性（冻结，A5/A17）**：每个 builtin 实例「声明为 direct 的工具原始名集合」== 其 `system_mcp_tools` 集合；`prepare_system_tools` 的提升路径与声明的 direct 必须一致，不得冲突（测试锁死）。
- 与 config hash：builtin 条目不参与 `server_config_hash`（`config.rs:151-187`）与 step 4 去重（`:388-407`）。必须有一条测试锁死「注入不改变既有 server 的 hash 去重结果」。
- 与 `save` 回写：`set_server_disabled` / `remove_server_from_config`（`config.rs:586` / `:478`）只修改**已存在于磁盘文件中的条目**；`{"web": {"disabled": true}}` 这类用户条目会正常写盘，但**内存里**的 builtin 默认条目永不被写盘。必须有一条测试断言：解析后调用两个写回函数，磁盘文件不出现「只有 builtin 才有的字段组合」（`system_mcp_tools` + 无 command/url + 名字为实例名）。
- 关闭语义（两条，必须都可观察）：
  1. 实例级（host 作用域）：规则 2/3 覆盖的两种用户写法（禁用 / 接管传输）。
  2. 会话级（turn 作用域）：见 IF-D10。
- 空集合短路（`initialize.rs:178-185`）在默认机器上不再触发：`Ready{total:0}` 路径改为「两个 in-process 实例的连接结果」。该行为变化必须写进 acceptance；走 loader 的既有测试按 §10 R11 处置。

### IF-D4 builtin 注册表（owner：E-01 数据 / E-02 解析与计算）

```rust
// peri-acp-types/src/builtin_mcp.rs —— 纯数据，无行为、无 sanitize 复刻
pub struct BuiltinMcpTool {
    pub original_name: &'static str,     // "WebSearch"（配置 / readiness / system_mcp_tools 侧精确匹配用它）
    pub effective_name: &'static str,    // 冻结字面量："mcp__web__WebSearch"（IF-D5；由 E-02 的测试与 sanitize 输出对齐）
    pub direct: bool,                    // IF-D13（A5）：Web 两工具 = true、Artifact = true；Cron/LSP 后续波次 = false
    pub prompt_declaration: Option<&'static str>, // A9：原样搬运既有模板（占位符渲染沿用既有规则）
}
pub struct BuiltinMcpInstance {
    pub name: &'static str,          // server name / 配置 key / 目录键："web" | "artifact"
    pub instance: &'static str,      // TransportConfig::Builtin.instance 身份（当前 == name）
    pub policy_key: &'static str,    // MetaHarness 关闭键："WebMiddleware" | "ArtifactMiddleware"
    pub tools: &'static [BuiltinMcpTool],
}
pub const BUILTIN_MCP_INSTANCES: &[BuiltinMcpInstance];                 // 仅**已实现**实例（wave 1 = web/artifact）
pub const BUILTIN_RESERVED_INSTANCE_NAMES: &[&str];                     // A3：web/artifact + 预留 cron/lsp/workspace
pub fn find(instance: &str) -> Option<&'static BuiltinMcpInstance>;     // 只命中已实现实例
pub fn is_reserved_instance_name(name: &str) -> bool;

/// IF-D15（A4）：effective name → 原始名，按**冻结字面量**查表。
/// 这是全部按名消费点的唯一归一入口；**禁止**反拆 `mcp__` 名字、**禁止**在本 crate 复刻 sanitize 规则。
/// 未命中（未知 / 外部 `mcp__*`）返回 None ⇒ 消费点沿用既有保守语义。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>;
```

```rust
// peri-middlewares/src/mcp/builtin/mod.rs —— 唯一行为实现
pub(crate) fn effective_tool_name(instance: &str, original_tool: &str) -> Option<String>;
pub(crate) fn declared_direct_tools(instance: &str) -> Option<&'static [&'static str]>; // IF-D13 与 system_mcp_tools 的一致性来源
pub(crate) fn is_declared_direct(server_name: &str, original_tool: &str) -> bool;        // 类型化 bridge 构造点用
pub(crate) fn builtin_prompt_declaration(server_name: &str, original_tool: &str) -> Option<&'static str>; // A9
pub(crate) fn apply_builtin_overlay(servers: &mut HashMap<String, McpServerConfig>, policy: &BuiltinInjectionPolicy);
pub(crate) fn closed_instances(disabled_middlewares: &HashSet<String>) -> BTreeSet<String>; // ← IF-D10
pub(crate) fn is_closed(server_name: &str, closed: &BTreeSet<String>) -> bool;
pub(crate) fn builtin_injection_policy_from_env() -> BuiltinInjectionPolicy; // 只在 load_merged_config_full 调用一次
```

- 冻结：`effective_tool_name` 必须调用 `tool_bridge::sanitize_name_component`（`tool_bridge.rs:56-66`）与同名模板，**不得**复制规则；`BuiltinMcpTool::effective_name` 是它的**冻结字面量**副本，必须由 E-02 的测试断言两者逐字相等（规则一份实现、字面量一份声明）。
- 冻结：`BUILTIN_MCP_INSTANCES` 是本批次「实例 / 原始工具名 / effective name / 逐工具 `direct` / `prompt_declaration` / 关闭键」的**唯一事实源**；`MIDDLEWARE_TOOL_NAMES`、`BUILTIN_INSTANCE_POLICY_KEYS`、关闭过滤、默认层注入、`system_mcp_tools`、IF-D15 的归一表、测试全部从它派生或与它对齐。
- 冻结：`original_tool_name_of_effective`（IF-D15）是**纯查表**，不得包含 sanitize / 反拆 / 大小写折叠逻辑；`peri-acp-types` 不得出现 `mcp__` 的字符串拼接或切分。
- 被它约束的测试：`cargo test -p peri-acp-types --lib -- builtin_mcp`（唯一性、非空、名字合法、保留名 ⊇ 已实现名、`policy_key` 唯一）；`cargo test -p peri-middlewares --lib -- mcp::builtin::tests`（三个冻结字面量与 `effective_tool_name` 输出一致，见 IF-D5；`direct` 集合 == `system_mcp_tools` 集合，见 A5/A17）。**挂载**：`peri-acp-types/src/builtin_mcp.rs` 内 `#[cfg(test)] #[path = "builtin_mcp_test.rs"] mod tests;`（owner E-01）；`mcp/builtin/mod.rs` 内 `#[cfg(test)] #[path = "builtin_test.rs"] mod tests;`（owner E-02）。**禁止**裸 `mcp::builtin` 过滤器（会命中 `builtin_spike_tests`）。

### IF-D5 模型面冻结名（owner：E-02）

冻结字面量（本轮由 `sanitize_name_component` + `mcp__{server}__{tool}` 模板推导，已核算）：

| 实例 | 原始工具名 | 模型面 effective name |
| --- | --- | --- |
| `web` | `WebSearch` | `mcp__web__WebSearch` |
| `web` | `WebFetch` | `mcp__web__WebFetch` |
| `artifact` | `artifact` | `mcp__artifact__artifact` |

- 必须有一条测试逐字断言上表（"sanitize 规则或模板漂移即红"）。
- 配置侧（`system_mcp_tools`）继续在**原始工具名**上精确匹配（IF-M2 不变）。同一张表逐工具携带 `direct`（IF-D13）与 `prompt_declaration`（A9），两者的来源分别是今天各工具的 `is_direct()`（`web_fetch.rs:95`、`web_search.rs:76`、`artifact/tool.rs:109`）与 `prompt_declaration()`（`web_fetch.rs:106-111`、`web_search.rs:87-92`、`artifact/tool.rs:121-127`）——必须逐字搬运，不得改写文本。
- **A4 的归一表由本表派生**：`original_tool_name_of_effective` 只索引上表三行（字面量 → 原始名）。表为空或名字不命中 → `None`（这就是「未知 / 外部 `mcp__*`」的判定入口，其保守语义分毫不变）。

### IF-D6 生效名归一与策略一致性层（owner：S-01；helper 归 E-01）

**唯一机制（A4，替代任何「逐点特判」写法）**：在**按名匹配的消费点**先取「候选名」，再用既有判定/匹配逻辑，而不是在每个函数里硬编码 effective name 字面量或前缀。

```rust
// peri-acp-types/src/builtin_mcp.rs（helper，IF-D15，owner E-01）
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>;

// 判定型消费点（替换语义）——peri-middlewares/src/permission/mod.rs、subagent/mod.rs
pub fn default_requires_approval(tool_name: &str) -> bool;  // 签名不变
pub fn is_edit_tool(tool_name: &str) -> bool;               // 签名不变
fn is_mutation_tool(name: &str) -> bool;                    // 签名不变
```

消费点全集（**冻结，共 7 处；不得新增第 8 处之外的临时特判**）：

| # | 消费点 | 语义 | owner |
| --- | --- | --- | --- |
| ① | `permission::default_requires_approval` | **判定型（替换）**：命中则用原始名走现有函数体（含 `starts_with("mcp__")` 前缀行） | S-01 |
| ② | `permission::is_edit_tool` | 判定型（替换） | S-01 |
| ③ | `subagent::is_mutation_tool` | 判定型（替换） | S-01 |
| ④ | TUI 按名分支 `kit/tool_display.rs:53,58`、`truncate.rs:186,190,275`（`kit/tool_semantics.rs`、`kit/acp_types/current_turn.rs` 经核查无按名分支） | **匹配型（原样优先，未命中再用原始名）** | S-08 |
| ⑤ | `peri-agent/src/tools/invocation.rs` 的 `TOOL_PARAM_ALIASES`（`:120-125`，匹配点 `:167-171`） | 匹配型 | S-02 |
| ⑥ | `peri-middlewares/src/hooks/matcher.rs` 的 `matches_matcher`（`:10`）/ `matches_if_condition`（`:32`） | 匹配型（matcher 模式侧与 tool_name 侧都按「原样优先、再用原始名」） | S-01 |
| ⑦ | `peri-agent/src/session/tool_catalog.rs::ToolFilterPolicy::canonical`（`:119`）的 allow/deny 过滤 | 匹配型 | S-02 |

冻结约束：
- 判定型必须**是替换语义**，不得写成「或运算 / 并集」——否则 `mcp__web__WebSearch` 会因 `mcp__` 前缀继续算 mutation，判定不再等于原始名（违反 IF-D6 的判定相等要求）。
- 匹配型必须**原样优先**：保证用户已写的 `mcp__web__*`（迁移后文档口径）与裸名 `WebFetch`（迁移前口径）两种写法都不失效。
- **未知 / 外部 `mcp__*` 的既有保守语义分毫不变**：`original_tool_name_of_effective("mcp__some_tool") == None` ⇒ `default_requires_approval` 仍为 `true`、`is_mutation_tool` 仍为 `true`。必须有反证测试。
- **事件载荷与 wire 仍暴露 effective name**：归一**只**用于按名判定/匹配，不得改写 `ToolCall` 载荷、ACP 事件、transcript 或 MCP wire 上的名字（投影真值不得被归一污染）。
- `sensitive_tool_entries()` **保持 14 项 / 3 条前缀**（顺序不变）；已迁移条目（`WebFetch` / `WebSearch`）的 `name` 改为 effective name（`mcp__web__WebFetch` / `mcp__web__WebSearch`），`mcp__` 前缀条目的 `description` 改写为「any MCP server tool (prefix match); builtin first-class capabilities follow their original tool's rule」；一致性测试（`permission/mod_test.rs:437-467`、`:472-482`）的探测名随之更新，计数断言必须仍为 14 / 3。`TOOL_WEBFETCH` / `TOOL_WEBSEARCH` 常量本身不改（仍有其它调用点）。
- 必须断言（approve/reject 双断言见 §8）：`default_requires_approval("mcp__web__WebSearch") == default_requires_approval("WebSearch") == true`；`default_requires_approval("mcp__artifact__artifact") == default_requires_approval("artifact") == false`；`default_requires_approval("mcp__some_tool") == true`；`is_mutation_tool("mcp__web__WebSearch") == is_mutation_tool("WebSearch") == false`；`is_mutation_tool("mcp__some_tool") == true`。
- **保留名的反例测试（A3，必须新增）**：构造「外部 server 名 `artifact` + 工具 `artifact`」与「外部 server 名 `web` + 工具 `WebSearch`」两种输入，断言要么**加载期失败**（保留名 typed error），要么 `default_requires_approval("mcp__artifact__artifact") == true`（即不被当作 builtin 一等工具放行）。这条测试同时是「parity 不得只按名字反查」的证据。
- 违反即回到主计划评审：若实施中决定**不做** parity（保留 `mcp__*` 一律敏感），必须同步改写 `mcp__` 条目 description 并在 acceptance 声明「artifact 上传新增审批门」为**有意的行为变更**，不得两种口径并存。

### IF-D7 静态表与投影同步（owner：S-02；TUI 面归 S-08）

**A. 两张名单（A7，冻结形态）**

| 表 | 内容 | 约束测试 |
| --- | --- | --- |
| `peri-acp-types/src/meta_harness.rs:93-121 MIDDLEWARE_NAMES` | **只保留链槽位名**：删除 `"WebMiddleware"`（`:107`）与 `"ArtifactMiddleware"`（`:118`），27 → 25 项 | `assembly_test.rs:1207-1217`（改写，见下） |
| `peri-acp-types/src/meta_harness.rs`（新增常量） | `pub const BUILTIN_INSTANCE_POLICY_KEYS: &[&str] = &["WebMiddleware", "ArtifactMiddleware"];`（builtin 实例关闭键，供 MetaHarness 关闭实例） | 同上 + 与声明表 `policy_key` 集合相等 |

`assembly_test.rs:1207-1217 middleware_names_match_production_blueprint` 的断言改为**强度不降**的新形态（三条同时成立）：
1. `{production_blueprint 槽位名} == MIDDLEWARE_NAMES`（集合相等；`slot_middleware_name` 的 `ChainSlot::Web`/`ChainSlot::Artifact` 两臂随之删除——`:1235`、`:1246`）；
2. `BUILTIN_INSTANCE_POLICY_KEYS == {BUILTIN_MCP_INSTANCES[].policy_key}`（集合相等；常量漂移或声明表漂移即红）；
3. `槽位名 ∩ 策略键 == ∅`（两表语义不重叠）。
> 等价表述（复核用）：`槽位名 ∪ 策略键 == MIDDLEWARE_NAMES ∪ BUILTIN_INSTANCE_POLICY_KEYS`——即「已知键全集」既不缺项也不重复，与改写前的集合相等断言**强度不降**。

`peri-acp/src/provider/config.rs` 必须同步（owner S-02）：
- `validate_meta_harness` 的 known 集合（`:400-405`）改为 `SECTION_IDS ∪ MIDDLEWARE_NAMES ∪ BUILTIN_INSTANCE_POLICY_KEYS ∪ BUILT_IN_SUBAGENTS_KEY`（否则 `"WebMiddleware": false` 会被当成未知键 warn + 忽略，静默丢失关闭语义）；
- `all_middleware_disabled`（`:419-426`）的判定面改为两表并集（否则「只剩两个 builtin 策略键为 false」不再触发全关告警）。

**B. 投影面（A4 归一后逐个落点）**

| 落点 | 变更 | 约束测试 |
| --- | --- | --- |
| `peri-acp-types/src/meta_harness.rs:163-201 MIDDLEWARE_TOOL_NAMES` | 删除 `"WebFetch"`、`"WebSearch"`、`"artifact"` 三项 | `assembly_test.rs:1255 middleware_tool_names_match_static_tool_sets`（同步改） |
| `peri-agent/src/session/exec/stage_builder/tools.rs:41-43` | 谓词语义重新论证：删除三个裸名后，该防御面不再覆盖 Web/Artifact；注释必须说明「builtin 工具以 `mcp__*` 形式存在于 MCP 目录，从不进入 `shared_tools`；裸名若由非 middleware 路径注册，**不得**再被本谓词剔除」 | `stage_builder/tools_test.rs`（新增裸名不被剔除的反向断言） |
| `peri-agent/src/session/exec/stage_builder/builder_v2_test.rs:38-70` | 其 fixture 用 `WebFetch` 作为「被剔除的 middleware 工具」样例，必须换成仍在表内的名字（如 `SkillTool`） | 同上 |
| `peri-acp/src/event/tool_projection.rs:61-70 infer_tool_kind` | **不新增字面量分支**：先经 `original_tool_name_of_effective` 归一，再走既有 `"WebFetch" \| "WebSearch" => ToolKind::Fetch`（`mcp__artifact__artifact` → `artifact` → 仍为 `Other`） | `cargo test -p peri-acp --lib -- event::mapper` |
| `peri-agent/src/tools/invocation.rs:120-125 TOOL_PARAM_ALIASES` | **不新增字面量键**：匹配点（`:167-171`）先按 `target.name()` 原样查表，未命中再用归一后的原始名查表（匹配型） | `cargo test -p peri-agent --lib -- tools::invocation` |
| `peri-middlewares/src/hooks/matcher.rs:10`/`:32` | 匹配型归一（模式侧与 tool_name 侧都「原样优先、未命中再用原始名」） | `cargo test -p peri-middlewares --lib -- hooks::matcher` |
| `peri-agent/src/session/tool_catalog.rs:119 ToolFilterPolicy::canonical` | 匹配型归一（allow/deny 的每个条目都按两个候选名比较） | `cargo test -p peri-agent --lib -- session::tool_catalog` |
| `peri-tui/src/kit/tool_display.rs:53`、`:58` 与 `truncate.rs:186`、`:190`、`:275` | 匹配型归一（**不得**把分支改成 `mcp__web__*` 字面量，A8） | `cargo test -p peri-tui --lib -- kit::tool_display`；`cargo test -p peri-tui --lib -- truncate` |

### IF-D8 槽位与挂载点删除（owner：I-03）

- 删除 `ChainSlot::Web`、`ChainSlot::Artifact`（`peri-agent/src/session/factory.rs:56-57`、`:82-83`）与 `production_blueprint` 中对应两项（`:112`、`:127`）。
- 删除挂载分支：`assembly.rs:272-275`、`:361-364`；`assembly/preparation.rs:131-133`；`assembly/workflow.rs:104-106`、`:205-207`（后者的替代交付见 IF-D10 第 2 条与 §10 R4：workflow agent 的 Web 能力由 builtin bridge 提供面接续，**不是**简单删除）。
- 保留其余槽位**相对顺序不变**：必须有断言「过滤掉被删两项后，本批次 blueprint 与上一批次 blueprint 逐项相等」。
- 工具面变化**显式冻结**（A6 修正后）：subagent `parent_tools` 与 workflow agent 工具面**不再含裸名 Web 工具，改为含 `mcp__web__*`（direct）**；`§10 R3/R4/R23` 与 §8 的对应断言必须落在可观察能力面。
- 被它约束的测试（全部要同步改，且改的是**期望值**不是断言强度）：`assembly_test.rs:394 blueprint_sequence_is_canonical`、`:453`/`:464 slot_name`、`:472 default_config_produces_canonical_chain`、`:505 artifact_middleware_can_be_disabled_independently`（改写为 builtin 实例关闭用例）、`:624 conditional_registration_matrix`、`:765 full_config_chain_order`、`:1094 meta_harness_disabled_tools_removed_from_chain`、`:1164 meta_harness_disabled_parent_tools_filtered`、`:1207-1217 middleware_names_match_production_blueprint`、`:1220-1249 slot_middleware_name`、`:1340 workflow_build_tools_filters_disabled`、`:1387 workflow_build_middlewares_filters_disabled`（后两条改为断言 builtin bridge 的存在/关闭，而不是 `WebMiddleware` 连坐）。

### IF-D9 两实例的 `system_mcp` 声明（owner：I-02）

- 两个 builtin 实例一律 `system_mcp = Some(true)`、`system_mcp_tools = Some(<该实例**声明为 direct**的原始工具名>)`（A5/A17；wave 1 三个工具全为 direct，故与「全部工具名」等价，但**以声明集合为准**，并由「`direct` 集合 == `system_mcp_tools` 集合」断言锁死）。
- 语义：启动前必须完成 transport / initialize / 能力协商 / live `tools/list`，随后提升为 direct 工具（IF-M2 的 `prepare_system_tools` + `McpToolBridge::with_direct`）；**声明的 direct（IF-D13）是同一次提升的声明式来源，两者不得冲突**。
- 紧急闸门（A2）：新增 `PERI_MCP_BUILTIN`（值 `off` / `0` → 不注入任何 builtin 实例；缺省 / 其它 → 注入全部，未知值 warn）。**含义必须写清**：off 的退回态**没有** Web/Artifact 能力（middleware 提供面已删，不存在旧实现回退路径），这是显式运维开关、不是静默降级；与 `disabled: true`（单实例禁用、其余不受影响）的区别见 §3 IF-D3。禁止把该 env 做成按工具粒度的策略开关；它只用于发布回退与支持场景。规则与 `PERI_MCP_APPS`（`apps.rs:216`）同层。
- 该接口直接对应 acceptance §4 的 **BLOCKED** 缺口：本批次必须交付「真正调用已提升为 direct 的 builtin 工具 + 同时观察审批与 wire」的用例（§8 第 4 行）。
- acceptance 必须记录：`PERI_MCP_BUILTIN=off` 时的能力面取证（两实例工具均不在首个 LLM 请求中）、以及「off 时能力不存在」这一**已知运维语义**（不得写成「回退到旧实现」）。

### IF-D10 能力关闭（owner：S-01 提供判定 / E-03、I-03 提供过滤落点）

冻结算法：

1. `closed_instances(&meta_harness.disabled_middlewares)`：遍历注册表，`policy_key ∈ disabled` 的实例进入关闭集（`policy_key` 的合法键集合 = `BUILTIN_INSTANCE_POLICY_KEYS`，见 IF-D7）。
2. 关闭集必须在**同一次 turn 的同一份 frozen policy** 下过滤**四个面**（ARC-CAPABILITY-CLOSURE-001；A6 后 workflow 面**不再**天然关闭）：
   - 启动提交的必需工具选择（`middleware.rs:440 startup_tool_update` 的 required 集合）→ 关闭实例的工具不得进入 direct 注入；
   - `mcp/middleware.rs::static_tool_bridges`（`:481-489`，含 `:487` fallback）→ 关闭实例的 bridge 不得进入 deferred 目录；
   - `assembly/preparation.rs:136`（parent_tools）→ 关闭实例的 bridge 不得进入 subagent 继承面；
   - **workflow agent 工具面（`assembly/workflow.rs::build_tools`，`:87`）**：本批次**要**把 builtin bridge 提供给 workflow agent（A6），因此关闭集必须在这里显式过滤——不得再用「workflow 天然不含 MCP bridge」作为关闭理由。
3. 关闭集**不得**影响 readiness：`await_system_connections` 仍按 `pool.configs`（pool 级）判定实例 ready（即「实例必须健康」与「本 turn 是否注入」是两件事）。这样处理可避免 `replace_static_mcp_tools` 的 `RequiredToolUnavailable`（`tool_catalog.rs` 内）把有意的关闭误报成启动失败。
4. 关闭的用户可观察面必须**同时**归零：首个 LLM 请求的 `tools`、ToolSearch deferred 摘要与检索结果、subagent `parent_tools`、**workflow agent 的工具列表**、TUI 工具卡片走通用路径（归一面本身不随关闭变化，属预期）、`10_hitl` 段落条文（静态描述，不随关闭变化，属预期）、TUI MCP 面板（面板仍显示实例为 connected —— 因为它是 pool 级事实，必须说明这是**有意**的语义分层）。
5. 必须交付 presence/absence 矩阵测试：对 `WebMiddleware=false` / `ArtifactMiddleware=false` / 两者都 false / `McpMiddleware=false` 四种输入断言上述**四个面**；关闭断言必须落在**可观察能力面**（首个 LLM 请求 tools / agent 工具列表 / 链工具集合），不得只断言中间量（如 `parent_tools` 内容）。

### IF-D11 `transport_type` 三分类（owner：E-03）

```rust
// peri-middlewares/src/mcp/client/status.rs（单一 helper，三处调用点共用）
fn transport_type_of(source: Option<&ConfigSource>, url: Option<&str>) -> &'static str
// Builtin → "builtin"；其余：url.is_some() → "http"，否则 "stdio"（逐位保持现状）
```

- 三个调用点 `status.rs:172`（handle 行）、`:203`（handle 行）、`:224`（config-only 行）必须全部改走该 helper；handle 行用 `h.source`，config-only 行用 `sc.source`。
- `peri-middlewares/tests/mcp_isolation_contract.rs:382-383` 的 `"stdio"` 断言**不得修改**，只复跑（其夹具是 stdio 实例，语义未变）。
- 新增断言：builtin 实例在 `all_server_infos()` 与 `snapshot()` 中 `transport_type == "builtin"`。

### IF-D12 reconnect 与 builtin 任务归属（owner：E-03）

- `reconnect` 的 builtin 分支：重新 `duplex` + `spawn` 一个**新的**同进程 server，重新走 `serve_client_auto`；旧 client service 关闭后旧 server task 靠 EOF 收敛（spike Q2 证据），不得依赖 `abort` 作为正常路径。
- pool 必须持有一个 builtin server task 表（与 `services` 同期登记 / 移除），在 pool 关闭与实例重连时按「有界等待 → 未收敛再 abort」处理（沿用 spike `shutdown` 模式 `:299-308`）。
- 冻结：wave 1 不新增 `McpTaskKey` 变体；若实施中发现必须走 keyed task，**必须**先在 §5 登记覆盖并说明 `task_scope` 语义（`stop_background` / `McpTaskShutdownReport`）如何保持。
- 证据断言：close / reconnect 后无 orphan task（`is_finished()` 在界内为真，或 abort 后 join 完成），且**不**打印或比较任何凭据。
- duplex 容量语义（A16）：`BUILTIN_DUPLEX_BUF = 8 * 1024`（sub-plan E 的 `mcp/builtin/runtime.rs`）的注释必须写成「capacity 只影响**背压**，不是单帧上限；单帧大小不受它约束」——不得再写「8 KiB 足够」。同时必须交付一条**大 payload 用例**（WebFetch 级正文，数十~数百 KB；见 §8 第 16 行），避免本批次就有的大结果无人验证。

### IF-D13 builtin 直连性声明（owner：E-01 数据 / E-03 生效点）

```rust
// peri-acp-types/src/builtin_mcp.rs（数据，E-01）
pub struct BuiltinMcpTool { /* … */ pub direct: bool, /* … */ }

// peri-middlewares/src/mcp/builtin/mod.rs（判定，E-02）
pub(crate) fn is_declared_direct(server_name: &str, original_tool: &str) -> bool;
pub(crate) fn declared_direct_tools(instance: &str) -> Option<&'static [&'static str]>;
```

- **生效点**：`build_typed_tool_bridges`（`tool_bridge.rs:412`）——遍历 pool 客户端时，若 `(client.name, tool.name)` 是已实现 builtin 实例的**声明为 direct** 的工具，则对该 bridge 应用 `.with_direct()`；其余一律保持 deferred。
- **未类型化版本行为逐位不变**：`build_tool_bridges`（`:430`，`pub`）必须改为调用新增的 `build_deferred_tool_bridges`（强制 deferred），使锁定测试 `tool_bridge.rs:503-528`（`typed[0].is_direct() == false` 且 `boxed[0].name() == typed[0].name()`）在 builtin 与外部 server 两种输入下都仍成立。`build_tool_bridges` 的调用点改走类型化版本（A6 面①②）。
- **语义（冻结）**：`direct = 声明的 direct || 启动期 system_mcp_tools 提升`；两者**不得冲突**（断言：声明 direct 集合 == `system_mcp_tools` 集合）。`system_mcp: true` 保留为**就绪闸门**、`system_mcp_tools` 保留为**启动期存在性校验**（既有语义不变，IF-M1/IF-M2）。
- wave 1 声明：`web` 的 `WebSearch`/`WebFetch` = `direct: true`、`artifact` 的 `artifact` = `direct: true`（对应今天各工具的 `is_direct()`）；Cron / LSP 后续波次的工具**预留为 `direct: false`**（deferred），本批次只登记不实现。
- 保留名（未实现）不参与判定：`is_declared_direct("workspace", "Read") == false`（`tool_bridge.rs:503-528` 的既有 fixture 用的是未实现的 `workspace`，因此该测试**不受**本接口影响；该测试也不经加载器，不受 A3 保留名错误影响）。

### IF-D14 builtin `call_tool` 的结果映射（owner：I-01）

按 spike 已跑通的形状（`peri-middlewares/src/mcp/builtin_spike_test.rs:209-218`）冻结，**二选一的写法一律作废**：

| 情形 | 返回 |
| --- | --- |
| 请求的工具名不在 handler 的工具集内 | `Err(McpError::invalid_params("unknown tool: {name}", None))`（spike `:210-215` 同形） |
| 工具执行 `Ok(text)` | `Ok(CallToolResponse::Complete(CallToolResult::success(vec![ContentBlock::text(text)])))`（spike `:216-218` 同形） |
| 工具执行 `Err(e)` | `Ok(CallToolResponse::Complete(CallToolResult::error(vec![ContentBlock::text(<分类化错误文本>)])))` —— **不用** `McpError::internal_error`；错误文本只含工具名与固定规则文本，不含路径 / env / 凭据（§9 规则 7） |
| 取消 / 超时 | 同 `Err` 分支（进模型可见的 error 结果），不 panic、不 abort |

- 必须有两条断言：成功形态（返回文本可辨认）与失败形态（错误进 `CallToolResult::error`，且未知工具名走 `invalid_params`）。
- 该接口与 IF-D11 的 `transport_type` 无关；与 IF-D9 的 fatal 路径的分工是：**启动期**失败 → fatal（readiness），**调用期**失败 → 模型可见的 error 结果（本接口）。

### IF-D15 生效名归一 helper（owner：E-01；消费点 owner 见 §3 IF-D6）

```rust
// peri-acp-types/src/builtin_mcp.rs —— 唯一归一入口（纯查表，不含 sanitize / 不含反拆）
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>;
```

- 数据来源 = IF-D5 的三个冻结字面量（由 E-02 的测试与 `effective_tool_name()` 输出对齐）；命中 ⇒ 返回原始名；未命中 ⇒ `None`（未知 / 外部 `mcp__*` 沿用既有保守语义）。
- 消费点全集 = §3 IF-D6 的 7 处（判定型替换、匹配型原样优先）；**禁止**在消费点就地硬编码 `mcp__web__*` 字面量或 `starts_with("mcp__")` 的新分支。
- **事件载荷与 wire 仍暴露 effective name**：本 helper 不得出现在 `ToolCall` 载荷构造、ACP 事件投影、MCP wire 序列化路径上（`original_tool_name()`（`tool_bridge.rs:200`）已明确「净化不可逆、不得反拆」）。


## 4. 文件所有权矩阵

**同一文件在同一时刻只能有一个 owner**（按 Wave 分时移交；跨 Wave 移交必须写在「owner」列）。违反所有权即为计划外改动，必须回退。

| 文件 / 目录 | owner | 备注 |
| --- | --- | --- |
| `peri-acp-types/src/builtin_mcp.rs`（新增）、`builtin_mcp_test.rs`（新增，挂载 `#[cfg(test)] #[path = "builtin_mcp_test.rs"] mod tests;`）、`peri-acp-types/src/lib.rs`（模块声明） | E-01 | 纯数据 + `find` + 保留名表 + IF-D15 helper；禁止 sanitize 复刻 |
| `peri-acp-types/src/plugin.rs` | E-01 | 仅新增 `ConfigSource::Builtin { instance }`；**不改** `McpServerConfig` 字段集 |
| `peri-middlewares/src/mcp/transport.rs`、`transport_test.rs` | E-01 | 新变体 + 三分类 + typed 未知实例错误 |
| `peri-middlewares/src/mcp/discover_tool.rs`、`discover_tool_test.rs` | E-01 | 新增 `config_source_str` 臂 |
| `peri-middlewares/src/mcp/builtin/mod.rs`（新增）、`mcp/builtin/builtin_test.rs`（新增；`mcp/builtin/mod.rs` 内挂 `#[cfg(test)] #[path = "builtin_test.rs"] mod tests;`） | **E-01（W1 骨架）→ E-02（W1 行为）→ I-01（W2 服务器子模块声明）** | 三个 Wave 串行移交，不得并发；字面量 / `direct` 集合 / 归一表测试落 `mcp::builtin::tests` |
| `peri-middlewares/src/mcp/builtin/runtime.rs`（新增）、`mcp/builtin/runtime_test.rs`（新增；内挂 `#[cfg(test)] #[path = "runtime_test.rs"] mod tests;`） | E-03 | spawn / duplex（A16 容量语义）/ task 归属 / close / reconnect |
| `peri-middlewares/src/mcp/builtin/web.rs`、`artifact.rs`（新增，各含 `#[cfg(test)]` 测试模块） | I-01 | 两个真实 `ServerHandler` + IF-D14 结果映射 |
| `peri-middlewares/src/middleware/web.rs`、`web_fetch.rs`、`web_search.rs`、`web_test.rs` | I-01 | 抽出核心操作供 builtin 复用；`is_direct` 提供面删除；`web_test.rs` 16 用例必须重挂载（sub-plan F §6.4） |
| `peri-middlewares/src/artifact/mod.rs`、`tool.rs`、`client.rs` | I-01 | 同上；`ArtifactClient` 需可注入 base url / token 以支持无网络测试 |
| `peri-middlewares/src/mcp/tool_bridge.rs`、`tool_bridge_test.rs` | **E-03** | IF-D13 生效点（类型化构造应用声明 direct）+ 新增 `build_deferred_tool_bridges`；未类型化版本行为逐位不变（锁定测试 `tool_bridge.rs:503-528` 不得改断言） |
| `peri-middlewares/src/mcp/middleware.rs`、`middleware_test.rs` | **I-03** | `static_tool_bridges`（`:481-489`，含 `:487` fallback）改类型化构造 + 关闭集过滤（A6 面①） |
| `peri-middlewares/src/mcp/initialize.rs`、`initialize_test.rs`、`reconnect.rs` | E-03 | 分支 / 超时 / 处理链（只消费 loader 结果，**不得**在此注入 builtin） |
| `peri-middlewares/src/mcp/client.rs`、`client/status.rs`、`client/lifecycle.rs` | E-03 | `transport_type_of` + builtin task 表 |
| `peri-middlewares/src/mcp/config.rs`、`config_test.rs` | I-02 | step 6.5 overlay + 策略参数收口（12 处 `_with_paths` 调用点）+ 覆盖规则 / 保留名 typed error / 关闭片段反例 / direct 一致性测试 |
| `peri-middlewares/src/mcp/mod.rs` | **E-01（W1：`mod builtin;` 声明）→ I-02（W3：crate 内测试模块挂载）** | 仓库约定：`*_test.rs` 必须在**实现模块**内挂 `#[cfg(test)] #[path]`；`mcp::builtin_apply`、`mcp::builtin_runtime` 两个测试模块由 I-02 一次挂载，避免两 owner |
| `peri-middlewares/src/assembly.rs`、`assembly/preparation.rs`、`assembly/workflow.rs`、`assembly_test.rs` | **I-03 唯一**（W3 单 owner，不并发） | 含 4 个挂载点 + `ChainSlot` 删除后的链序断言 + A6 的三个工具面（parent_tools 改类型化构造、workflow 补 builtin bridge 提供面） |
| `peri-acp/src/host/assemble.rs` | **I-03**（W3） | A6③ 的池注入点：`:510-511` 改 `default_workflow_middleware_factory_with_pool(mcp_pool_concrete.clone())`（`mcp_pool_concrete` 在 `:332` 定义） |
| `peri-agent/src/session/factory.rs` | **I-03 唯一** | `ChainSlot` + `production_blueprint`；**该文件没有任何测试模块**（A14）⇒ 对应断言挂 `assembly::tests` 与 `session::exec::stage_builder`，不得写 `-- session::factory` |
| `peri-middlewares/src/middleware/mod.rs`、`peri-middlewares/src/lib.rs` | I-03 | 删除 `WebMiddleware` 再导出（该文件唯一 owner） |
| `peri-acp-types/src/meta_harness.rs` | S-02 | `MIDDLEWARE_TOOL_NAMES` 删三裸名 + `MIDDLEWARE_NAMES` 删两键 + 新增 `BUILTIN_INSTANCE_POLICY_KEYS` + 注释 |
| `peri-acp/src/provider/config.rs`、`config_test.rs` | **S-02** | known 集合与 `all_middleware_disabled` 改为两表并集（A7） |
| `peri-agent/src/session/exec/stage_builder/tools.rs`、`tools_test.rs`、`stage_builder/builder_v2_test.rs` | S-02 | 谓词语义再论证 + fixture 换名 |
| `peri-agent/src/tools/invocation.rs`、`invocation_test.rs` | S-02 | 参数别名表（A4 ⑤ 匹配型归一） |
| `peri-agent/src/session/tool_catalog.rs` | S-02 | `ToolFilterPolicy::canonical`（A4 ⑦ 匹配型归一） |
| `peri-acp/src/event/tool_projection.rs`、`mapper_test.rs` | S-02 | `infer_tool_kind` 走归一（A4） |
| `peri-middlewares/src/tool_search/declaration.rs`、`declaration_test.rs` | **S-02**（W3） | A9：声明段仍产出（`declaration_test.rs:239-242` 不再调用 `WebMiddleware::build_tools()`）；命令 `cargo test -p peri-middlewares --lib -- tool_search::declaration` |
| `peri-middlewares/src/permission/mod.rs`、`mod_test.rs` | S-01 | 归一（判定型 ①②）+ `sensitive_tool_entries` 条目名与 description + 段落条文 |
| `peri-middlewares/src/subagent/mod.rs`、`mod_test.rs` | S-01 | `is_mutation_tool` 归一（判定型 ③） |
| `peri-middlewares/src/hooks/matcher.rs` | S-01 | `matches_matcher` / `matches_if_condition` 的匹配型归一（A4 ⑥） |
| `peri-middlewares/src/subagent/built-in/web-researcher.md` | **S-03**（W3） | A19：frontmatter `tools:` 与正文点名改用 effective name |
| `peri-acp/prompts/sections/01_intro.md`、`11_subagent.md` | **S-06**（W1，纯文本） | A19：prompt 段落点名同步 |
| `peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md` | **S-05**（W1，纯文本） | A19：内置 skill 文本 |
| `peri-acp/src/prompt/prompt_test.rs` | **S-04**（W3） | A19：`10_hitl` 段落渲染与 `sensitive_tool_entries()` 同步（`prompt_test.rs:227-247` 既有断言不改） |
| `peri-tui/src/kit/tool_display.rs`、`truncate.rs`（+ 各自自带测试 `kit/tool_display_test.rs`、`truncate_test.rs`，若挂载点不同以实际模块为准） | **S-08**（W3） | A8：按名分支走归一 helper，**不得**硬编码 `mcp__web__*`；`kit/tool_semantics.rs`、`kit/acp_types/current_turn.rs` 经核查无按名分支，本波不改 |
| `peri-middlewares/src/mcp/builtin_apply_test.rs`（新增，crate 内） | I-02 | 默认层注入 / 覆盖与禁用语义 / 保留名 typed error / 关闭片段反例 / 写回隔离 / hash 无关性 / direct 一致性 |
| `peri-middlewares/src/mcp/builtin_runtime_test.rs`（新增，crate 内） | V-01 | 启动路径 / 审批 approve+reject / wire 计数 / 关闭矩阵 / 无 orphan / 大 payload（A16）/ 隔离可观察断言（A13） |
| `peri-acp/src/host/mod.rs`、`peri-acp/src/host/mcp_v4_builtin_test.rs`（新增）、`peri-acp/src/host/mcp_v4_wire_fixture.rs`（**新增夹具**：node 脚本含 wire 日志与 `tools/call` 分支 + 复刻的工具调用 model 替身 + 1 条自检测试） | V-02（**W0 建夹具并录基线 → W4 使用**） | A10/A12：夹具唯一 owner；`mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT` 无 wire 日志、无 `tools/call`（`:101` 落 `-32601`）**不得**复用；`PtcScriptedModel`（`executor_flow_test.rs:2153`）是私有 struct ⇒ 需复刻 |
| `peri-middlewares/tests/mcp_isolation_contract.rs`、`tests/mcp_host_policy_contract.rs` | V-03 | 既有断言**不改**；**允许的唯一改动** = `EnvIsolation`（`:140-160`）增加 `PERI_MCP_BUILTIN=off` + 新增一条「off 时 pool 恰有两台 server」断言（§5 R28 登记） |
| `peri-acp/src/host/mcp_v4_startup_test.rs` | V-02 | 11 个用例：`HomeRedirect`（`:107+`）增加 `PERI_MCP_BUILTIN=off` + 新增一条「off 时 servers 恰为夹具实例」断言（§5 R28 登记）；**不改既有断言** |
| `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（新增） | **V-06（W0：迁移前基线小节）→ V-04（W5：终态）** | 现场验收记录；三态列 + 未验证项清单模板（含 A2/A13 的 UNVERIFIED） |
| `docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 路由表、`docs/reference/mcp-ecosystem.md` | V-05 | 依赖 V-04 终态；含 ARC-CAPABILITY-CLOSURE-001 / ARC-MIDDLEWARE-001 条目更新 + 名称 / 关闭 / hook / 过滤的用户面说明；doc 检查项 = `git diff --check` + 链接检查 + 人工核对清单 |

**不属于本批次任何 task 的文件**：`side-projects/local-mcp-server/**`（wave 3）、`docs/design/**`（设计文档不回填）。`peri-tui/**` **属于**本批次（A8：S-08 持有 2 个源文件 + 测试）。

## 5. 对 sub-plan 的覆盖登记

| 编号 | 覆盖内容 | 以何为准 |
| --- | --- | --- |
| R1 | builtin 实例身份经 `ConfigSource::Builtin { instance }` 传递；**覆盖** sub-plan E 若提出的 `TransportConfig::for_server(name, config)` 新签名方案 | 本文件 IF-D2 + §3 IF-D1 |
| R2 | **默认层注入点唯一 = loader 的 step 6.5**（`mcp/config.rs` 的 `load_merged_config_full_with_paths`：step 6 循环 `:419-430` 之后、step 7 `validate_config` `:433` 之前），策略 `BuiltinInjectionPolicy` 为显式参数、只在 `load_merged_config_full`（`:301`）读一次 `PERI_MCP_BUILTIN`；**覆盖** sub-plan E 若提出的「在 `initialize_config`（`initialize.rs:146`）内注入或读 env」与任何「env-gated 旁路 = 只有 on 才注入」的写法（A1） | 本文件 IF-D3 + §9 规则 11 |
| R3 | 覆盖规则的**唯一表述**是 §3 IF-D3 的六条：既**不**做「整条 key 替换」、也**不**做字段级深合并；规则 2 在 `disabled != Some(true)` 时同时填 `system_mcp` / `system_mcp_tools`（A17），规则 3 对保留名报加载期 typed error（A3），规则 5 冻结唯一合法关闭片段（A18）；**覆盖** sub-plan E/F 中任何「整条不动 / 用户接管传输 / 只填 source」的旧口径 | 本文件 IF-D3 + §5 R14/R20/R30/R31 |
| R4 | `sensitive_tool_entries()` 保持 14 项 / 3 前缀，只改 `mcp__` 条目文本；**覆盖**「新增一条 builtin 例外条目」的方案（会破 `permission/mod_test.rs:472-482`） | 本文件 IF-D6 |
| R5 | **两张名单（A7，取代「保留两键」的旧口径）**：`MIDDLEWARE_NAMES` 只留链槽位名（**删** `WebMiddleware` / `ArtifactMiddleware`）；新增 `BUILTIN_INSTANCE_POLICY_KEYS` 承载这两个关闭键；`assembly_test.rs:1207-1217` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == 声明表 `policy_key` 集合」∧「交集为空」；**覆盖**任何「两键留在 `MIDDLEWARE_NAMES` 并改注释」或「删键且不新增策略表」的方案（后者会让 `"WebMiddleware": false` 变成 warn+忽略的未知键，静默丢失关闭语义） | 本文件 IF-D7 A 节 + §5 R15/R24 |
| R6 | 关闭过滤必须落在**四个面**（启动提交的 required 集合 / `mcp/middleware.rs::static_tool_bridges` / `preparation.rs:136` parent_tools / **workflow agent 工具面**）并以注册表 `policy_key` 为唯一映射；**覆盖**任何按字符串硬编码 `mcp__web__` 前缀的过滤实现，也**覆盖**「workflow 天然不接入 ⇒ 天然关闭」的旧假设（A6） | 本文件 IF-D10 + §5 R23 |
| R7 | 跨层「首个 LLM 请求 tools」与 fatal 路径断言归 **V-02**（`peri-acp` host seam）；crate 内只断言可观察层（沿用 part-1 R9） | 本文件 §4 + §8 |
| R8 | 「真正调用已提升为 direct 的 builtin 工具 + 同时观察审批与 wire」用例归 **V-01**（crate 内，`with_direct` 是 `pub(crate)`，`tests/` 集成层无法升级 bridge）；**覆盖**任何把该断言放在 `peri-middlewares/tests/**` 的方案 | 本文件 IF-D9 + §8 第 4 行 |
| R9 | **workflow agent 工具面必须保留 Web 能力（A6，覆盖旧 R9 的「不接入」结论）**：`build_tool_bridges` 的生产调用点只有 `preparation.rs:136` 与 `middleware.rs:487`，因此 workflow agent 今天不含任何 MCP bridge；本批次必须在 `assembly/workflow.rs::build_tools`（`:87`）补 builtin bridge 提供面（最小改法见 §10 R4），否则删除 `:104-106` 后 workflow agent 会**净失去** Web 能力；**覆盖**任何「workflow agent 不会获得 `mcp__web__*`」或「能力等价性断言 = 两者都不含」的表述 | 本文件 §2 + §10 R4 + §5 R23 |
| R10 | E-01 / E-02 / I-01 分三个 Wave 串行移交 `mcp/builtin/mod.rs`；**覆盖** sub-plan E 若把三件事放在同一 Wave 的三个并发 agent | 本文件 §4 + §7 |
| R11 | 若必须新增 `McpTaskKey::Builtin`，须先在本表登记并说明 `task_scope` 语义 | 本文件 IF-D12 |
| R12 | 紧急闸门 `PERI_MCP_BUILTIN` 是**发布回退开关**，不得成为按工具粒度的策略面 | 本文件 IF-D9 |
| R13 | **覆盖 sub-plan F 的 IF-F2**：不新增 `McpServerConfig.builtin` 字段、不做 struct literal 收口；builtin 身份由既有的 `source`（`#[serde(skip)]`）承载 | 本文件 IF-D1 + IF-D2 |
| R14 | **覆盖 sub-plan F 的 IF-F1 覆盖规则**：采用 §3 IF-D3 的六条（缺失→插入完整条目；无 command/url→填 `source`，且 `disabled != Some(true)` 时同时填 `system_mcp`/`system_mcp_tools`；**声明 command/url 的保留名 → 加载期 typed error**；非法关闭片段 → 加载期拒绝；direct 集合 == `system_mcp_tools` 集合）。由此 `{"web": {}}` 仍是**可用且 direct** 的 builtin 实例，`{"web": {"disabled": true}}` 注册为 `Disabled` 而非 `Failed`，`{"web": {"disabled": true, "system_mcp": true}}` 被拒绝；F 若坚持 `builtin == Some("web")` 的不变式，须改判为「`source == Some(ConfigSource::Builtin{..})`」 | 本文件 IF-D3 + §5 R20/R30/R31 |
| R15 | **覆盖 sub-plan F 的 IF-F4/F6**：`MIDDLEWARE_NAMES` **删除** `"WebMiddleware"` / `"ArtifactMiddleware"` 两键（改为只含链槽位名），并**新增** `BUILTIN_INSTANCE_POLICY_KEYS` 承载这两个关闭键；因此**不需要**遗留键别名表。F 的 I-08（`provider/config.rs` 的 known 集合）**不再取消**：该文件由 **S-02** 持有并必须把 known 集合与 `all_middleware_disabled` 改为两表并集（A7） | 本文件 IF-D7 A 节；`peri-acp/src/provider/config.rs:400-405`/`:419-426` |
| R16 | **覆盖 sub-plan F §6.7 与 sub-plan H §6.4**：`transport_type` **必须**新增 `"builtin"`（三分类 helper），不是「另立 issue」。依据：面板/`discover_tool` 需要区分 builtin 与 stdio；`mcp_isolation_contract.rs:382-383` 是 stdio 夹具，helper 保持其余分支逐位不变即可不破断言 | 本文件 IF-D11 |
| R19 | **任务编号仲裁（H）与证据分工**：sub-plan H 使用 `V-00…V-08`，与本文件 `V-01…V-06` 语义不同，执行时以本文件 §6 为准。映射：**H-V-00（前置核实 U3 等只读复核）并入本文件 V-06**（W0）；H-V-01（host 启动/fatal）→ V-02；H-V-02（首个 LLM 请求 tools）→ V-02；H-V-03（审批 + wire 的 BLOCKED 缺口闭合）→ **V-06 的 host 夹具（wire 日志 + `tools/call`）在 V-02 复证 + V-01 crate 内主证据**，二者都必须存在（A10）；H-V-03b（builtin 实例自身的 wire）若 E 未提供 per-instance wire 观测面则记 UNVERIFIED；H-V-04（实例隔离）→ **V-01**（断言改按 A13 的可观察四项，不落 `tests/` 集成层）；H-V-05/V-06（关闭矩阵 / parent_tools 与 workflow 工具面）→ V-01；H-V-07/V-08 → V-04；H 的额外测试文件与 `peri-acp/src/host/mod.rs` 的单 owner 串行约束**照用** | 本文件 §6 + §8 |
| R17 | **覆盖 sub-plan G 的 S-01 可选分支 O1**（把 `artifact` 加进敏感清单）：**缺省不启用**，parity 是唯一缺省语义；若启用 O1，按 IF-D6 末条要求同步 `sensitive_tool_entries()`、段落与 acceptance 声明 | 本文件 IF-D6 |
| R18 | **任务编号仲裁**：sub-plan F 使用 `I-01…I-09`，与本文件 `I-01…I-03` 语义不同。执行时以本文件 §6 为准，F 的编号按此映射：F-I-01（声明表）→ E-01（数据）+ E-02（行为）；F-I-02（`ConfigSource` + 新字段 + `TransportConfig`）→ E-01（**去掉新字段部分**）；F-I-03（默认层 overlay）→ I-02；F 的第 4/5 号 task（两个 handler）→ I-01；F-I-06（删 middleware 与挂载点）→ I-01 + I-03；F-I-07（槽位/名单/锁定测试）→ I-03 + S-02；F-I-08（遗留键保留）→ **改判为 S-02 的两表改造**（见 R15）；F-I-09（声明段投影）→ S-02（A9 后是「声明必须仍产出」，不是缺口登记）。**F/G/H 文本里出现的旧装配编号（数值 4）一律读作 `I-03`**（A14） | 本文件 §6 |
| **R20** | **保留实例名（A3）**：`web`/`artifact` 为保留名，另预留 `cron`/`lsp`/`workspace`；用户为保留名声明 `command`/`url` → 加载期 typed error（`McpConfigError::ReservedBuiltinInstanceName`）；必须交付反例测试（外部 `artifact` server + `artifact` 工具） | 本文件 IF-D3 规则 3 + IF-D4 保留名表 + §8 第 9 行 |
| **R21** | **生效名归一原则（A4）**：单一 helper `original_tool_name_of_effective`（落 `peri-acp-types`）在 7 个消费点统一；判定型 = 替换、匹配型 = 原样优先；**禁止**逐点硬编码 effective name 或新增 `mcp__` 前缀分支；事件载荷与 wire 仍暴露 effective name；未知 `mcp__*` 保守语义不变（反证测试） | 本文件 IF-D6 + IF-D15 + §8 第 10 行 |
| **R22** | **直连性声明（A5）**：逐工具 `direct` 在 `build_typed_tool_bridges` 生效；未类型化 `build_tool_bridges` 行为逐位不变（新增 `build_deferred_tool_bridges`），其调用点改走类型化版本；声明 direct 集合 == `system_mcp_tools` 集合 | 本文件 IF-D13 + §8「投影同步」行 |
| **R23** | **三个工具面能力等价（A6）**：主链 / 子 agent 继承 / workflow agent 三面都必须保留 Web 能力并可观察断言；**覆盖**任何「只改一到两个面」或「workflow 天然关闭」的写法 | 本文件 IF-D10 第 2 条 + IF-D8 + §10 R4 + §8 第 11 行 |
| **R24** | **两表形态（A7）**：`MIDDLEWARE_NAMES` 删两键、新增 `BUILTIN_INSTANCE_POLICY_KEYS`；`provider/config.rs` 的 known 集合与 `all_middleware_disabled` 改两表并集（owner S-02） | 本文件 IF-D7 A 节 + §5 R5/R15 |
| **R25** | **TUI 在范围内（A8）**：`peri-tui` 的 `kit/tool_display.rs`、`truncate.rs` 走归一 helper（不得硬编码 `mcp__web__*`），owner **S-08**；理由 = ARC-CAPABILITY-CLOSURE-001（`docs/standards/architecture-contracts.md:63`）明文列举 TUI completion | 本文件 §4 + §6 S-08 行 + §7 W3 |
| **R26** | **声明段不得丢失（A9）**：机制 = 声明表携带 `prompt_declaration` 模板 + builtin direct 桥实现 `prompt_declaration()`；`{{name}}` 按既有规则渲染为 effective name；`tool_search/declaration.rs`+`declaration_test.rs` 归 **S-02**；必须有「声明仍产出」断言 | 本文件 §4 + §6 S-02 行 + §8 第 12 行 |
| **R27** | **V-03 可达性（A10）**：新建 host 侧 wire 夹具（含 wire 日志与 `tools/call` 分支，owner **V-02**，文件 `peri-acp/src/host/mcp_v4_wire_fixture.rs`）；`PtcScriptedModel` 私有 ⇒ **需复刻**；`mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT` 不得复用 | 本文件 §4 + §6 V-06 行 |
| **R28** | **隔离夹具 env 改动（A11，登记为允许改动）**：`tests/mcp_isolation_contract.rs` 与 `peri-acp/src/host/mcp_v4_startup_test.rs` 的夹具守卫增加 `PERI_MCP_BUILTIN=off`，**既有断言一字不改**，并各新增一条「off 时无 builtin 实例」断言 | 本文件 §4 两行 + §7 W4 闸门 + §8 第 13 行 |
| **R29** | **`call_tool` 结果映射冻结（A15）**：IF-D14（owner I-01）；指向已作废 IF-E2 的引用一律改指本接口 | 本文件 IF-D14 |
| **R30** | **`{"web": {}}` 语义（A17）**：规则 2 在 `disabled != Some(true)` 时同时填 `system_mcp` + `system_mcp_tools`，保证可见性等价（不出现 direct→deferred 的静默降级） | 本文件 IF-D3 规则 2 + IF-D9 |
| **R31** | **关闭片段形状冻结（A18）**：唯一合法写法 `{"web": {"disabled": true}}`；`disabled + system_mcp: true` 组合必须被加载期拒绝（不得落到 `readiness.rs` 的 fatal），并有反例用例 | 本文件 IF-D3 规则 5 + §8 第 17 行 |
| **R32** | **子计划 G 未登记任务的 owner 与波次（A19）**：S-03（`web-researcher.md`，W3）、S-04（`10_hitl` 段落 + `prompt_test.rs`，W3）、S-05（内置 skill 文本，W1）、S-06（prompt 段落 `01_intro.md`/`11_subagent.md`，W1）、S-08（TUI，W3）在本文件 §4/§6 全部登记；S-10（`docs/reference/mcp-ecosystem.md`）归 **V-05**（W5） | 本文件 §4 + §6 + §7 |
| **R33** | **`sensitive_tool_entries()` 条目名（A19）**：`WebFetch`/`WebSearch` 两条目的 `name` 改为 effective name，**保持 14 项 / 3 前缀与顺序不变**，一致性测试探测名同步；`10_hitl` 渲染列表的断言改为「含两个 web effective name、不含裸名、不含 `artifact`」 | 本文件 IF-D6 + §8「策略一致性」行 |
| **R34** | **实现期接口细化（W1 闸门登记）**：`apply_builtin_overlay` 实际签名为 `Result<(), BuiltinOverlayError>`，IF-D4 冻结签名无返回类型——二者不可同时成立。取前者：IF-D3 规则 3/5 的「加载期 typed error」无其它返回通道。错误类型为 crate 内 `pub(crate) enum BuiltinOverlayError`（变体 `ReservedBuiltinInstanceName` / `DisabledWithSystemMcp`，文本只含实例名），**I-02** 在 `mcp/config.rs` step 6.5 1:1 映射进 `McpConfigError::{ReservedBuiltinInstanceName, BuiltinClosureFragmentInvalid}`（与 R20 冻结名一致），不泄漏公共 API | 本文件 IF-D3 规则 3/5 + IF-D4 + R20 |
| **R35** | **两处 §4 矩阵外文件的 owner 补充（实现期登记，登记后视为矩阵内）**：① `peri-acp/src/session/frozen.rs`（`build_meta_harness_state` 的 `false` 键判定面改为 `MIDDLEWARE_NAMES ∪ BUILTIN_INSTANCE_POLICY_KEYS`，owner = **S-02**）——只读一张表会让 `"WebMiddleware": false` 退化为「键存在但无效果」，正是 IF-D10 / ARC-CAPABILITY-CLOSURE-001 判失败的中间态；② `peri-middlewares/src/assembly/mcp.rs`（`add_mcp` 追加 `.with_builtin_closures(...)`，owner = **I-03**）——IF-D10 面①/② 的关闭集注入落点。两者均为 A7 / IF-D10 的直接落点，不是计划外重构 | 本文件 IF-D7 A 节 + IF-D10 + §9 规则 10 |
| **R36** | **死文件清理归收口阶段**：`peri-middlewares/src/middleware/web.rs` 在实现期被保留（I-01 按 sub-plan F §8 不得先删提供面；I-03 未接手），W3 后它已不在模块树（`grep -rn 'mod web' peri-middlewares/src/middleware/` = 0 命中）、不参与编译。收口时删除该文件，使 sub-plan F §1 判据 2「`WebMiddleware` 提供面消失」在文件系统层面亦成立；`web_fetch.rs` / `web_search.rs` 保留为 `pub(crate)` 内部实现供 builtin handler 复用，其 `is_direct()` 已无活消费点（删除属卫生项，本批次不做，记入 acceptance §11） | 本文件 §4 `middleware/web.rs` 行 + §6 I-03 行 |
| **R37** | **wire 夹具文件名后缀（交付期登记，收口闸门发现）**：A10 的 host 侧 wire 夹具在计划与 §4 矩阵中记作 `peri-acp/src/host/mcp_v4_wire_fixture.rs`，而 `scripts/check-layer-imports.sh` 的测试豁免按**路径子串**判定（`TEST_EXEMPTS="_test.rs _test/ tests/"`，`:30`、`:62`），该夹具引用业务 crate（`peri_middlewares` / `peri_model`，与 `mcp_v4_startup_test.rs:35-36`、`executor_flow_test.rs:45-47` 同形）必须落在豁免集内。**实际交付文件名为 `peri-acp/src/host/mcp_v4_wire_fixture_test.rs`**，经 `#[path]` 挂载保持模块名 `host::mcp_v4_wire_fixture` 不变 ⇒ 过滤器、三条用例名、观察量与 §2 基线记录逐字不变。计划/子计划文本中的旧名按计划期命名保留，以本行为准 | 本文件 §4/§6 V-06/V-02 行 + sub-plan H §2 |

## 6. 任务表

任务 ID 前缀 = sub-plan 归属：`E-`（builtin 运行时）、`I-`（Web/Artifact 实例与装配）、`S-`（策略与投影）、`V-`（验证与文档）。`→` 表示必须完成后才能开始。

| 批次 | Task | 标题 | owner 产出文件 | 依赖 | 验证命令 |
| --- | --- | --- | --- | --- | --- |
| W0 | **E-00** | 基线闸门（只读，无写入） | 无 | — | `cargo check --workspace --all-targets`；`cargo test -p peri-middlewares --lib -- mcp::builtin_spike`（应 6 passed） |
| W0 | **V-06** | **迁移前基线与 host 侧 wire 夹具（A10/A12）**：建 `peri-acp/src/host/mcp_v4_wire_fixture.rs`（node 脚本含 **wire 日志** + `tools/call` 分支；**复刻**工具调用 model 替身，因 `PtcScriptedModel`（`executor_flow_test.rs:2153`）私有；自带 1 条自检测试）；在**迁移前 HEAD** 录「首个 LLM 请求的工具名」基线（应为裸名 `WebSearch`/`WebFetch`/`artifact`，三者迁移前均 direct） | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（新增，挂载于 `peri-acp/src/host/mod.rs`）；`spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（**新建**，「迁移前基线」小节） | — | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture`（≥1 passed，非 0 tests）；基线以命令输出 + 计数写入 acceptance |
| W1 | **E-01** | 类型与身份：`ConfigSource::Builtin{instance}`、`TransportConfig::Builtin`、三分类超时、注册表数据（含逐工具 `direct`（IF-D13）/ `prompt_declaration`（A9）/ 保留名表（A3）/ IF-D15 helper）、`mcp/mod.rs` 模块声明 | `peri-acp-types/src/builtin_mcp.rs`、`builtin_mcp_test.rs`（新）、`lib.rs`、`plugin.rs`；`peri-middlewares/src/mcp/transport.rs`、`transport_test.rs`、`discover_tool.rs`、`discover_tool_test.rs`、`mcp/mod.rs` | E-00 → | `cargo check --workspace --all-targets`；`cargo test -p peri-acp-types --lib -- builtin_mcp`；`cargo test -p peri-middlewares --lib -- mcp::transport` |
| W1 | **E-02** | 注册表行为（纯函数）：`effective_tool_name`、`apply_builtin_overlay`（含保留名 typed error 与关闭片段反例）、`BuiltinInjectionPolicy`、`closed_instances`、`declared_direct_tools` / `is_declared_direct`、冻结字面量与 direct 一致性测试 | `peri-middlewares/src/mcp/builtin/mod.rs`、`mcp/builtin/builtin_test.rs`（新） | E-01 →（同一 agent 连续执行） | `cargo test -p peri-middlewares --lib -- mcp::builtin::tests`（**禁止**裸 `mcp::builtin`，会命中 `builtin_spike`） |
| W1 | **S-05 / S-06** | 文本面（纯文本，可与 W1 并行）：内置 skill 文本；prompt 段落点名同步 | `peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md`（S-05）；`peri-acp/prompts/sections/01_intro.md`、`11_subagent.md`（S-06） | — | `git diff --check` + `cargo test -p peri-acp --lib -- prompt::tests`（回归，不证明文本正确） |
| W2 | **E-03** | 运行时：duplex spawn（A16 容量语义）、builtin task 表、`initialize_config`/`reconnect` 的 builtin 分支、`transport_type_of`、有界关闭、**IF-D13 生效点**（类型化构造 + `build_deferred_tool_bridges`） | `peri-middlewares/src/mcp/builtin/runtime.rs`、`runtime_test.rs`（新）、`mcp/tool_bridge.rs`、`tool_bridge_test.rs`、`mcp/initialize.rs`、`initialize_test.rs`、`mcp/reconnect.rs`、`mcp/client.rs`、`mcp/client/status.rs`、`mcp/client/lifecycle.rs` | E-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin::runtime`；`cargo test -p peri-middlewares --lib -- mcp::initialize`；`cargo test -p peri-middlewares --lib -- mcp::tool_bridge`（锁定测试不得改断言） |
| W2 | **I-01** | 两个 builtin server：web（`WebSearch`/`WebFetch`）、artifact（`artifact`），自 `middleware/web_*`、`artifact/*` 抽出可复用核心操作；**IF-D14 结果映射**；`builtin/mod.rs` 追加子模块声明 | `peri-middlewares/src/mcp/builtin/web.rs`、`artifact.rs`、`mcp/builtin/mod.rs`、`middleware/web.rs`、`web_fetch.rs`、`web_search.rs`、`web_test.rs`、`artifact/mod.rs`、`artifact/tool.rs`、`artifact/client.rs` | E-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin::web`；`cargo test -p peri-middlewares --lib -- mcp::builtin::artifact`（须含 IF-D14 成功/失败两形态） |
| W2 | **S-01** | 策略一致性层：A4 归一（判定型 ①②③ + 匹配型 ⑥）+ `sensitive_tool_entries()` 条目名/description + 保留名反例测试 | `peri-middlewares/src/permission/mod.rs`、`mod_test.rs`、`subagent/mod.rs`、`mod_test.rs`、`hooks/matcher.rs` | E-02 → | `cargo test -p peri-middlewares --lib -- permission::tests`；`cargo test -p peri-middlewares --lib -- subagent::tests`；`cargo test -p peri-middlewares --lib -- hooks::matcher` |
| W3 | **I-02** | step 6.5 overlay + 覆盖规则（含 A17/A3/A18 四条派生规则）+ 策略参数 + 写回隔离 + crate 内验证 + 测试模块挂载 | `peri-middlewares/src/mcp/config.rs`、`config_test.rs`、`mcp/builtin_apply_test.rs`（新增）、`mcp/mod.rs`（测试挂载） | E-02 → | `cargo test -p peri-middlewares --lib -- mcp::config::tests`；`cargo test -p peri-middlewares --lib -- mcp::builtin_apply` |
| W3 | **I-03** | 链与挂载点：删除两槽位与 4 个挂载点、`lib.rs`/`middleware/mod.rs` 再导出、全部装配测试期望值同步、**A6 的三个工具面**（`static_tool_bridges` 改类型化构造 + 关闭过滤；`preparation.rs:136` 改类型化构造；`workflow.rs::build_tools` 补 builtin bridge 提供面 + `assemble.rs:510-511` 传池） | `peri-middlewares/src/assembly.rs`、`assembly/preparation.rs`、`assembly/workflow.rs`、`assembly_test.rs`、`mcp/middleware.rs`、`middleware/mod.rs`、`lib.rs`、`peri-agent/src/session/factory.rs`、`peri-acp/src/host/assemble.rs` | I-02 → | `cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-agent --lib -- session::exec::stage_builder`（**不得**写 `-- session::factory`：该文件无测试模块，会 0 tests 判失败） |
| W3 | **S-02** | 投影与静态表同步（两表 + `provider/config.rs` 键集合 + meta_harness / tool_projection / TOOL_PARAM_ALIASES（⑤）/ `ToolFilterPolicy::canonical`（⑦）/ stage_builder 谓词）+ **A9 声明段保留** | `peri-acp-types/src/meta_harness.rs`、`peri-acp/src/provider/config.rs`、`config_test.rs`、`peri-agent/src/session/exec/stage_builder/tools.rs`、`tools_test.rs`、`builder_v2_test.rs`、`peri-agent/src/tools/invocation.rs`、`invocation_test.rs`、`peri-agent/src/session/tool_catalog.rs`、`peri-acp/src/event/tool_projection.rs`、`mapper_test.rs`、`peri-middlewares/src/tool_search/declaration.rs`、`declaration_test.rs` | I-03 → | `cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-middlewares --lib -- tool_search::declaration`；`cargo test -p peri-agent --lib -- session::exec::stage_builder`；`cargo test -p peri-agent --lib -- tools::invocation`；`cargo test -p peri-acp --lib -- event::mapper` |
| W3 | **S-03 / S-04** | 文本与段落面：内置 agent `web-researcher.md`（frontmatter + 正文点名）；`10_hitl` 段落与 `prompt_test.rs` 同步 | `peri-middlewares/src/subagent/built-in/web-researcher.md`（S-03）；`peri-acp/src/prompt/prompt_test.rs`（S-04） | S-01 → | `cargo test -p peri-acp --lib -- prompt::tests`（既有 `test_hitl_section_rendered_by_holder` 不改断言）；S-03 为人工核对 + `git diff --check` |
| W3 | **S-08** | TUI 按名分支走 A4 归一（**不得**硬编码 `mcp__web__*`） | `peri-tui/src/kit/tool_display.rs`、`truncate.rs`（+ 各自测试模块） | S-01 → | `cargo test -p peri-tui --lib -- kit::tool_display`；`cargo test -p peri-tui --lib -- truncate` |
| W4 | **V-01** | crate 内启动路径 + 审批 approve/reject + wire 计数 + 关闭矩阵（**四**面）+ 无 orphan + 大 payload（A16）+ **隔离可观察断言（A13）** | `peri-middlewares/src/mcp/builtin_runtime_test.rs`（新增） | I-02、I-03、S-01、S-02 → | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` |
| W4 | **V-02** | host seam：首个 LLM 请求含 builtin direct 工具（与 W0 基线对照）、用户 MCP 配置共存、fatal 路径、BLOCKED 缺口复证（用 V-06 的 wire 夹具） | `peri-acp/src/host/mod.rs`、`peri-acp/src/host/mcp_v4_builtin_test.rs`、`peri-acp/src/host/mcp_v4_wire_fixture.rs`（V-06 产出，本 task 复用）、`peri-acp/src/host/mcp_v4_startup_test.rs`（仅夹具 env + 新增断言，A11/R28） | V-01 → | `cargo test -p peri-acp --lib -- host::mcp_v4_builtin`；`cargo test -p peri-acp --lib -- host::mcp_v4_startup` |
| W4 | **V-03** | 隔离与宿主策略回归复跑（既有断言只复跑；**允许的唯一改动** = 夹具 env 加 `PERI_MCP_BUILTIN=off` + 新增 off 断言，见 R28） | `peri-middlewares/tests/mcp_isolation_contract.rs`（夹具 env + 新增断言） | V-01 → | `cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1`；`cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` |
| W5 | **V-04** | 验收记录（**新建**文件，不追加到 v4-part-1 记录）：三态列 + PARTIAL/BLOCKED/UNVERIFIED + 命令与计数 + **未验证项清单模板**（必须含：A13 的 capability root / 凭据隔离、A2 的 `PERI_MCP_BUILTIN=off` 运维语义、builtin 工具体内 cancel 响应、真实网络与上传）；**保留 V-06 的「迁移前基线」小节原样** | `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md`（owner 传递：V-06 → V-04） | W1–W4 全部 → | 复跑 W1–W4 全部命令并记录终态（含 V-06 基线命令的对照重跑） |
| W5 | **V-05** | code-index / 标准口径 / 路由表同步（含链序与关闭面条目）+ **用户面文档 `docs/reference/mcp-ecosystem.md`**（A19：名称 / 关闭语义 / hook / `--disallowed-tools` / 保留名 / `PERI_MCP_BUILTIN`） | `docs/code-index/**`、`docs/standards/**`、`CLAUDE.md`、`docs/reference/mcp-ecosystem.md` | V-04 → | `git diff --check` + 链接检查 + 人工核对清单（doc 检查项：ARC-CAPABILITY-CLOSURE-001 / ARC-MIDDLEWARE-001 条目、保留名与关闭片段说明、hook 匹配规则） |

## 7. 波次与并发（每波集成闸门）

| Wave | 并发 task | 同 crate 冲突 | 串行原因 / 闸门检查项 |
| --- | --- | --- | --- |
| **W0** | E-00、V-06 | — | 起始 `cargo check --workspace --all-targets` 必须绿；`mcp::builtin_spike` 6 用例必须绿（builtin 可行性的现场前提）；**V-06 必须在任何生产代码改动之前**在迁移前 HEAD 录下「首个 LLM 请求工具名」基线（A12）——否则「可见性等价」不可证伪。闸门：基线行写入 acceptance 且可复核 |
| **W1** | E-01、E-02（**同一 agent 连续执行**）、S-05/S-06（纯文本，可并行） | `peri-acp-types` + `peri-middlewares` | E-01 落 `TransportConfig` 变体后，`initialize.rs:275` / `reconnect.rs:90` 立刻编译失败；E-02 不恢复编译，但两者共享 `mcp/builtin/mod.rs`，故必须同 agent。闸门：`cargo check --workspace --all-targets` 绿（W1 结束时允许 E-03 尚未接线，但**不允许**穷尽 match 未收口）+ `mcp::builtin::tests` 绿（字面量 / direct 一致性 / 保留名 / 关闭片段反例） |
| **W2** | E-03、I-01、S-01 | `peri-middlewares` 三个并发 | 文件互斥（§4）；三者的公共文件 `mcp/mod.rs`、`mcp/builtin/mod.rs` 已在 W1 定稿，W2 只由 I-01 追加子模块声明。闸门：`mcp::initialize` / `mcp::builtin::runtime` / `mcp::tool_bridge` / `permission::tests` / `subagent::tests` / `hooks::matcher` 六组测试全绿 |
| **W3** | I-02 → I-03（串行）、S-02、S-03/S-04、S-08 | `peri-middlewares`（I-02、I-03）+ `peri-agent`/`peri-acp`（S-02、S-04）+ `peri-tui`（S-08） | I-03 **必须**在 I-02 之后：先让 builtin 注入可用，再删除 middleware 提供面，否则出现能力真空窗口；S-02/S-03/S-04/S-08 依赖 I-03 或 S-01 的判定。闸门：`mcp::config::tests`、`mcp::builtin_apply`、`assembly::tests`、`tool_search::declaration`、`session::exec::stage_builder`、`tools::invocation`、`event::mapper`、`prompt::tests`、`kit::tool_display`、`truncate` 全绿 + `cargo check --workspace --all-targets` 绿 |
| **W4** | V-01、V-02、V-03（V-02/V-03 在 V-01 之后） | `peri-middlewares`（V-01、V-03）+ `peri-acp`（V-02） | V-02 需要 V-01 证明的 crate 内事实，并复用 V-06 的 wire 夹具。闸门：三组命令全绿，且 `mcp_isolation_contract` / `mcp_host_policy_contract` / `mcp_v4_startup` 的**既有断言未改**仍绿，仅夹具 env（`PERI_MCP_BUILTIN=off`）与新增 off 断言发生变化（R28） |
| **W5** | V-04、V-05（串行） | — | V-04 收口后 V-05 才动文档；V-05 必须保留 V-06 写下的「迁移前基线」小节原样（不得改写基线）。闸门：`git diff --check` 绿 + 设计文档 mtime 未变（未回填）+ `docs/reference/mcp-ecosystem.md` 与 `docs/code-index/**` 的 doc 检查项（链接检查 + 人工核对清单）完成 |

## 8. 验收矩阵（诚实分级）

| 验收项 | 可观察断言（必须写明「什么被观察到」） | 承担测试文件 | 命令 | 证据强度口径 |
| --- | --- | --- | --- | --- |
| **迁移前基线（A12，可见性等价的前置）** | 在**迁移前 HEAD** 上、用 V-06 的同一 host 夹具录下首个 LLM 请求的工具名集合（应为裸名 `WebSearch`/`WebFetch`/`artifact`，三者迁移前均 direct：`web_fetch.rs:95`、`web_search.rs:76`、`artifact/tool.rs:109`），与迁移后（三个冻结 effective name）逐项对照写进 acceptance | `peri-acp/src/host/mcp_v4_wire_fixture.rs`（V-06 自检测试）+ acceptance「迁移前基线」小节 | `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture`（≥1 passed） | **强**：同一夹具、同一观察量、两个时点；缺此行则「可见性等价」不可证伪（A12） |
| **契约 5 扩展：builtin 实例隔离（A13 的口径）** | ①两个实例的 `Arc<McpClientHandle>` 非同一（`!Arc::ptr_eq`）且 `McpConnectionKey` 不同；②重连 `web` 后 `web` 的 generation 递增而 `artifact` 不变；③关闭 `web` 后 `artifact` 仍能完成一次真实 `tools/call`；④若 E 提供 per-instance wire 观测面（`#[cfg(test)]` 暴露 duplex 读半或 method 序列）则断言 wire 不串，否则该子项**记 UNVERIFIED**；⑤namespace 路由正确（`mcp__web__*` 只落 `web` 的 handler） | `mcp/builtin_runtime_test.rs`（crate 内）+ 复跑 `tests/mcp_isolation_contract.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime`；`cargo test -p peri-middlewares --test mcp_isolation_contract -- --test-threads=1` | **中强**：crate 内可触达 `pub(crate)` seam。**capability root 与凭据在 builtin 形态下不可证伪**（`capability_profile` 是 pool 级 `mcp/apps.rs:229-268`、`bind_execution_cwd` 是 pool 级 `initialize.rs:161`）⇒ 必须按 UNVERIFIED 口径声明，**不得**写成同义反复（如「两个实例构造参数不同」） |
| **契约 6 缺口闭合：被提升为 direct 的 builtin 工具走完整审批链（approve）** | 模型侧工具名 = `mcp__web__WebSearch`（direct，来自真实启动路径）；审批 broker 收到的 name 是该 effective name；**批准后** 内置 server 的 `call_tool` 计数恰好 +1，且工具结果内容可辨认 | `mcp/builtin_runtime_test.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime` | **强（crate 内）**：这是 acceptance §4 BLOCKED 缺口的正面闭合——真实调用 + 同时观察审批与 wire |
| **契约 6 缺口闭合（reject）** | 同一链路：**拒绝后** 内置 server 的 `call_tool` 计数为 0，工具结果/错误为拒绝语义，且不发生第二次审批 | 同上 | 同上 | **强（crate 内）** |
| **策略一致性** | `default_requires_approval("mcp__web__WebSearch") == true == 原始名`；`default_requires_approval("mcp__artifact__artifact") == false == 原始名`；`default_requires_approval("mcp__some_tool") == true`；`is_edit_tool` 与 `is_mutation_tool` 各自同构断言；`sensitive_tool_entries()` 仍 14 项 / 3 前缀且与判定一致 | `permission/mod_test.rs`、`subagent/mod_test.rs` | `cargo test -p peri-middlewares --lib -- permission`；`-- subagent` | **强**：parity 是纯函数判定，可逐条断言；破坏面由既有锁定测试兜底 |
| **能力关闭（ARC-CAPABILITY-CLOSURE-001）** | 四组输入（`WebMiddleware=false` / `ArtifactMiddleware=false` / 两者都 false / `McpMiddleware=false`）下同时断言**四个面**：首个模型请求 `tools`、ToolSearch deferred 摘要与检索、subagent `parent_tools`、**workflow agent 工具列表**的对应变化；并断言**未知键**与**合法键但无实例**两种输入不同（前者 warn+忽略） | `mcp/builtin_apply_test.rs`（crate 内）+ `assembly_test.rs` + `peri-acp/src/host/mcp_v4_builtin_test.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_apply`；`cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | **中强**：direct / deferred / parent_tools / workflow 工具列表在 crate 内可断言；「首个 LLM 请求」必须在 host seam（V-02）。断言必须落在可观察能力面，不得只断言中间量 |
| **默认层与覆盖语义（含 A17/A3/A18）** | 注入后 `pool.configs` 含两实例且 `system_mcp == Some(true)`、`system_mcp_tools == 声明 direct 集合`；`{"web": {}}` → 仍为 system 依赖且 direct（A17）；`{"web": {"disabled": true}}` → `Disabled` 且不 fatal；`{"web": {"disabled": true, "system_mcp": true}}` → 加载期被拒绝；保留名（`web`/`artifact`/`cron`/`lsp`/`workspace`）声明 `command`/`url` → typed error；builtin 注入不改变既有 server 的 hash 去重结果；两个写回函数不把 builtin 写盘；`BuiltinInjectionPolicy::none()`（= `PERI_MCP_BUILTIN=off`）时零注入 | `mcp/builtin_apply_test.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_apply` | **强**：配置构造 + 磁盘断言都是纯函数 / 本地文件可枚举 |
| **builtin 运行时（含失败/超时/取消）与 duplex 容量（A16）** | modern 握手（`server/discover`，无 `initialize`）且 `peer_info()` 为 `Some`；`tools/list` 成功提交证据；连接失败 → `Failed` 且无 ready 证据；超时 → timeout 分类正确；pool 关闭 → 无 ready、无 orphan task；reconnect → 新代证据且旧句柄不再被接受；**大 payload**（WebFetch 级正文，数十~数百 KB）经 builtin 实例往返成功且 body 完整 | `mcp/builtin_runtime_test.rs`、`mcp/initialize_test.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_runtime`；`cargo test -p peri-middlewares --lib -- mcp::initialize` | **强（crate 内）**；启动 fatal 的用户可见投影（模型调用 0 次、ACP `-32000`+`kind=internal`）归 V-02。`BUILTIN_DUPLEX_BUF` 只影响背压，不设帧上限（A16） |
| **投影同步（A4 归一）** | `infer_tool_kind("mcp__web__WebFetch") == ToolKind::Fetch`（经归一，非硬编码）；`search_term → query` 别名对 effective name 生效（经归一）；TUI 的 WebSearch/WebFetch 卡片仍有参数摘要；`MIDDLEWARE_TOOL_NAMES` 不再含三个裸名且与链工具集合一致；`槽位名 == MIDDLEWARE_NAMES` ∧ `策略键 == policy_key 集合` ∧ 交集为空；删除两槽位后 blueprint 相对顺序不变 | `mapper_test.rs`、`invocation_test.rs`、`assembly_test.rs`、`peri-tui` 测试 | `cargo test -p peri-acp --lib -- event::mapper`；`cargo test -p peri-agent --lib -- tools::invocation`；`cargo test -p peri-middlewares --lib -- assembly::tests`；`cargo test -p peri-tui --lib -- kit::tool_display` | **强**：全部为可枚举断言 |
| **声明段保留（A9）** | 迁移后 `collect_declarations` 对三个 builtin direct 工具**仍产出**声明文本；渲染后的 `{{name}}` 为 effective name（`mcp__web__WebFetch` / `mcp__web__WebSearch` / `mcp__artifact__artifact`），文本不含裸名 | `peri-middlewares/src/tool_search/declaration_test.rs` | `cargo test -p peri-middlewares --lib -- tool_search::declaration` | **中强**：文本等价通过「仍产出 + 渲染名字正确」断言；逐字不漂移由声明表与既有模板搬运保证 |
| **保留名反例（A3）** | 外部 server 名 `artifact` + 工具 `artifact`（或 server 名 `web` + 工具 `WebSearch`）⇒ 要么加载期失败（保留名 typed error），要么 `default_requires_approval("mcp__artifact__artifact") == true`（不被当作 builtin 一等工具放行） | `permission/mod_test.rs`（+ 配置侧 `mcp/builtin_apply_test.rs`） | `cargo test -p peri-middlewares --lib -- permission::tests`；`cargo test -p peri-middlewares --lib -- mcp::builtin_apply` | **强**：两种结果都可断言，是「parity 不得只按名字反查」的证据 |
| **`PERI_MCP_BUILTIN=off` 语义（A2）** | off 时两实例的工具**均不在**首个 LLM 请求的 `tools`、也不在 deferred 目录；off ≠ 回退到 middleware 实现（middleware 提供面已删） | `mcp/builtin_apply_test.rs` + `peri-acp/src/host/mcp_v4_builtin_test.rs` | `cargo test -p peri-middlewares --lib -- mcp::builtin_apply`；`cargo test -p peri-acp --lib -- host::mcp_v4_builtin` | **强（能力面断言）**；「off 时能力不存在」必须写进 acceptance 与 `docs/reference/mcp-ecosystem.md`（显式运维开关，不是静默降级） |
| **契约 7 文档纪律** | acceptance 三态列齐备 + 未验证项清单（含 A2 off 语义、A13 capability root / 凭据 UNVERIFIED）；builtin 实例与 `local-mcp-server` 的「目标归属 ≠ 已落地」区分明确；设计文档未被回填；`docs/reference/mcp-ecosystem.md` 与 `docs/code-index/**` 已同步且 doc 检查项完成 | `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md` | `git diff --check` + 链接检查 + 人工核对 mtime | **强**（依赖 V-04/V-05 的诚实标记） |

## 9. 全局施工规则（所有 agent 必须遵守）

1. **文件所有权**：只修改 §4 / §6 中列为你产出的文件。编译错误出现在**非你拥有**的文件时，忽略并如实报告；不得顺手修复。
2. **测试模块 wiring**：新增 `*_test.rs` 必须在对应生产模块内挂载 `#[cfg(test)] #[path = "<name>_test.rs"] mod tests;`（模块名参与 `cargo test` 过滤者沿用其既有命名，如 `mcp/mod.rs` 的 `builtin_spike_tests`）。未挂载的测试文件不会被编译。
3. **禁止假绿**：验证命令输出 `0 tests` 视为**失败**。完成报告必须包含实际测试数与 exit status。
4. **禁用过宽测试过滤器**：不得用过宽前缀。**禁用清单**：`mcp::builtin`（会命中 `builtin_spike_tests`）、`permission`（会命中 `permission::auto_classifier`）、`subagent`（会命中 `subagent::fork`）。**必须使用**：`mcp::builtin::tests`、`mcp::builtin::runtime`、`mcp::builtin::web`、`mcp::builtin::artifact`、`mcp::builtin_apply`、`mcp::builtin_runtime`、`mcp::builtin_spike`、`mcp::transport`、`mcp::initialize`、`mcp::tool_bridge`、`mcp::config::tests`、`permission::tests`、`subagent::tests`、`hooks::matcher`、`tool_search::declaration`、`assembly::tests`、`session::exec::stage_builder`、`tools::invocation`、`event::mapper`、`session::tool_catalog`、`host::mcp_v4_builtin`、`host::mcp_v4_wire_fixture`、`host::mcp_v4_startup`、`host::prompt::tests`、`prompt::tests`、`kit::tool_display`、`truncate`（模块名以真实模块为准）。
   **特别禁止** `-- session::factory`：`peri-agent/src/session/factory.rs` **没有任何测试模块**，该过滤器必然 0 tests 而按规则 3 判失败；此文件的相关断言改挂 `assembly::tests` 与 `session::exec::stage_builder`。
5. **不并发跑 `cargo`**：同 Wave 内多 agent 会争抢 target 锁；等待即可，不得 kill 他人构建。
6. **不新增 public API**：除 §3 明确冻结者外，新增 seam 一律 `pub(crate)`。`peri-acp-types` 的注册表与 IF-D15 helper 是冻结的 `pub`（纯数据 / 纯查表）。
7. **错误信息与 fixture 不含任何真实 secret**：错误只保留实例名 / server 标识 / 阶段类别与固定规则文本；不打印 env、headers、URL 认证信息、OAuth 值、`PERI_ARTIFACTS_TOKEN`。测试使用注入的本地 base url 与假 token。
8. **不回填设计文档**：`docs/design/mcp-adaptation-v4-part-1.md` 不写批次、提交号、勾选状态；本批次新事实只进 acceptance 与本计划。
9. **不把未验证项写成已验证**：`PARTIAL` / `BLOCKED` / `UNVERIFIED` 必须显式标注；绿色局部单测不得升级为整体迁移结论。**强制 UNVERIFIED 的两项**：① capability root 隔离与凭据隔离在 builtin 形态下不可证伪（A13）② `PERI_MCP_BUILTIN=off` 的退回态没有 Web/Artifact 能力（A2，是运维语义而非缺陷）。
10. **关闭语义与配置形状不得降级**：任何「键仍存在但不再生效」的中间态一律判定失败（IF-D10）；`{"web": {"disabled": true, "system_mcp": true}}` 必须被加载期拒绝（A18），保留名的 `command`/`url` 接管必须报 typed error（A3）——两者都不得以「文档已说明」代替实现。
11. **生效名归一只能走单一 helper**：任何按名判定 / 匹配的落点都必须调用 `original_tool_name_of_effective`（IF-D15）；**禁止**在消费点硬编码 `mcp__web__*` 字面量、`starts_with("mcp__web__")` 或自建第二张反查表（A4/A8）。归一不得泄漏到事件载荷、transcript 与 wire（仍为 effective name）。
12. **新增模块必须配对测试与挂载**：每个新增 `*.rs` 模块都要有 `*_test.rs` 并在实现模块内挂 `#[cfg(test)] #[path = "..."] mod tests;`（或按仓库约定命名，如 `mcp/mod.rs` 的 `builtin_apply_tests`），且 §4 已登记 owner；未挂载的测试文件不会被编译，等于假绿。

## 10. 风险与回退

| 风险 | 影响 | 缓解 / 回退 |
| --- | --- | --- |
| **R1 新 transport 变体的穷尽 match 连带** | `initialize.rs:275`、`reconnect.rs:90`、`status.rs:172/203/224`、`discover_tool.rs:331` 全部编译失败；若 W1 未收口，整个 crate 无法验证 | E-01 与 E-02 由同一 agent 连续执行；W1 闸门强制 `cargo check --workspace --all-targets` 绿；不允许「先加变体、稍后收口」跨 Wave |
| **R2 builtin server 覆写 `discover` 导致 `tools/list` 被拒** | 实例连得上但发现必失败 → system 闸门 fatal，所有 session 无法启动 | 硬约束：builtin server **不得**覆写 `discover`，必须用 rmcp 默认实现（spike Q1(b)/Q3(b) 证据 `builtin_spike_test.rs:634-680`）；`initialize_test.rs:3-25` 的「fake peer 拒绝 discover 逼 legacy」夹具**不得**用于 builtin 路径（那会掩盖该风险）；V-01 必须含一条「无 `initialize` 帧、`tools/list` 成功」的线路级断言 |
| **R3 subagent 继承面的可见性漂移** | 迁移前 subagent 从 `parent_tools` 拿到 **direct** 裸名 Web 工具；`build_parent_tools`（`preparation.rs:123-140`）当前既给裸名 direct（`:131-133`）又给 `build_tool_bridges(pool)` 的**全 deferred** bridge（`:136`），而子 agent 链**没有 ToolSearch**（唯一实例化点 `assembly.rs:356`）⇒ 若只做删除，子 agent 会**净失去** Web 能力 | 已按 **A6 面②** 冻结：`preparation.rs:136` 改走**类型化构造**（`build_typed_tool_bridges` + 声明的 direct，IF-D13），使 `mcp__web__*` 在子 agent 侧仍为 direct；断言落在可观察能力面（子 agent 工具的 `is_direct()` / 首个请求 tools），**不得**只断言 `parent_tools` 里的名字 |
| **R4 workflow agent 工具面收缩（A6 面③，最高优先修复）** | workflow agent 的 `build_tools`（`workflow.rs:87`）与 `build_middlewares`（`:151`）只有 `WebMiddleware`、**没有** `McpMiddleware`；删除 `:104-106` / `:205-207` 后其 Web 能力**彻底消失**（净损失，不是等价迁移） | **最小改法（冻结）**：`WorkflowAgentMiddlewareFactory` 由 ZST 改为持有 `Option<Arc<McpClientPool>>`；新增 `default_workflow_middleware_factory_with_pool(Option<Arc<McpClientPool>>) -> Arc<dyn WorkflowMiddlewareFactory>`（旧 `default_workflow_middleware_factory()` 签名与行为不变，内部传 `None`，既有装配测试因此不需改）；`build_tools` 追加 builtin direct bridge（经 `closed_instances(disabled)` 过滤）；`build_middlewares` 删除 `WebMiddleware` 分支且**不**新增 MCP middleware；调用点 `peri-acp/src/host/assemble.rs:510-511` 传 `mcp_pool_concrete.clone()`（该变量在 `:332` 定义，`bare` 时为 `None`）。owner **I-03**；断言 = workflow agent 的工具列表含 `mcp__web__*`（未关闭时）且不含裸名，关闭 `WebMiddleware` 时不含 |
| **R5 builtin 失败导致全局启动阻塞（最严重）** | 两实例是 `system_mcp = true`；任一连接/发现/必需工具校验失败即 fatal（`middleware.rs:703-716`），影响每个 session | ① 紧急闸门 `PERI_MCP_BUILTIN`（IF-D9；**语义为 A2**：off 时没有该能力，不是回退到旧实现）② V-01/V-02 覆盖 fatal 路径专项断言 ③ W0 基线含 spike 复跑 + V-06 迁移前基线 ④ 发布前必须真机跑「用户 MCP 配置共存 + 空配置」两种 host seam |
| **R6 审批语义漂移** | `default_requires_approval` / `is_mutation_tool` / 段落条文三处不一致，用户看到「段落说敏感但不再弹窗」或反之；外部同名 server 还可借 parity 继承「无需审批」（A3） | IF-D6 的单一归一机制 + 判定表断言 + `sensitive_tool_entries` 一致性测试 + **保留名 typed error 与反例测试**（A3）；若选择不做 parity，必须在 acceptance 声明为有意变更，禁止两种口径并存 |
| **R7 `transport_type` 断言失效** | `mcp_isolation_contract.rs:382-383` 期望 `"stdio"` | 该断言只覆盖 stdio 夹具，helper 必须逐位保持非 builtin 分支行为；V-03 复跑且**既有断言不改**；夹具 env 改动（`PERI_MCP_BUILTIN=off`）与新增 off 断言已在 §5 R28 登记 |
| **R8 `system_mcp` 语义被误读** | 把「pool 级 ready」理解成「turn 级注入」→ 关闭语义被静默破坏 | IF-D10 第 3 条显式分层；关闭矩阵测试同时断言 ready 仍成立而注入为零。capability root / 凭据隔离**不得**据此声称已验证（A13：按 UNVERIFIED 记录） |
| **R9 默认层与既有 hash 去重互相干扰** | 注入位置若前移到 step 4 去重之前，builtin 条目进入 `manual_hashes`，可能误删用户插件 server | IF-D3 冻结「step 6.5 注入」（在 `:388-407` 去重之后），并有「不改变既有去重结果」断言 |
| **R11 既有测试因新增两个 in-process 实例而变红** | step 6.5 注入后，走 `run_initialize` / `#[cfg(test)] initialize` / loader 的测试会多出两个 server（计数、`init_status`、`configs`、`all_server_infos` 断言受影响） | 处置顺序：① 先确认差异确由注入造成（命令 + 输出）② 由该测试文件的 owner 决定「更新期望值」或「显式传 `BuiltinInjectionPolicy::none()`」，**或**对走生产 `run_initialize` 的集成夹具按 §5 R28 设 `PERI_MCP_BUILTIN=off`（只改夹具 env，既有断言不动）③ 命令直读 loader 的单元测试改传策略参数。**禁止**为了让旧测试变绿而把生产注入改成旁路（例如改成只有 env=on 才注入）——那会让默认路径与测试路径分叉 |
| **R12 保留名接管被静默接受（A3）** | 外部 server 占用 `web`/`artifact` 名字后，parity 会按名字反查而放行，静默移除 `mcp__*` 审批门 | 加载期 typed error（`McpConfigError::ReservedBuiltinInstanceName`）+ 反例测试（外部 `artifact` server + `artifact` 工具）；错误文本只含实例名 |
| **R13 非法关闭片段导致全局 fatal（A18）** | `{"web": {"disabled": true, "system_mcp": true}}` 会走到 `readiness.rs:466-479` 的 `Err(Disabled)`，阻断**所有** session | 冻结唯一合法关闭片段（只写 `disabled: true`）+ 加载期拒绝该组合 + 反例用例；用户文档（`docs/reference/mcp-ecosystem.md`，owner V-05）同步 |
| **R14 归一被绕过或泄漏（A4/A8）** | 某消费点漏改 → 判定/匹配与原始名不一致（工具卡片退化为通用路径、别名失效、hook 不触发）；或把归一写进事件载荷 / wire → 投影真值被污染 | §3 IF-D6 的 7 个消费点全集 + §9 规则 11（禁止第二张反查表 / 禁止硬编码字面量）+ §8「投影同步」行断言 + 反证测试（未知 `mcp__*` 不变） |
| **R15 duplex 容量被误当帧上限（A16）** | 大结果（WebFetch 级正文）被当成「超出 8 KiB 就失败」，或注释误导实施者加缓冲逻辑 | 注释改为「capacity 只影响背压，不是帧上限」；V-01 加大 payload 用例（数十~数百 KB） |
| **R10 `local-mcp-server` 被误当作本批次对象** | 误改独立 workspace 的 `Cargo.toml` / 加 members → 根 workspace 被污染 | §2 末行事实登记 + §4「不属于任何 task 的文件」清单 |

## 11. 与上一批次的关系

**复用的冻结接口（不得改语义）**：

| 复用 | 本批次用法 |
| --- | --- |
| IF-M1（`system_mcp` / `system_mcp_tools` / `system_mcp_timeout` 与三变体校验） | builtin 实例的声明方式；不新增字段、不改 serde |
| IF-M2（`prepare_system_tools` / `with_direct` / 原始名精确匹配 / all-or-nothing） | builtin 工具提升为 direct 的唯一路径；本批次不改其签名与语义 |
| IF-M3（`DiscoveryEvidence` / 严格 discovery） | builtin 实例同样必须走真实 live `tools/list` 才提交 ready |
| IF-M5（`StartupToolUpdate` / catalog 原子提交） | builtin 工具的目录发布路径；本批次只增加「关闭集过滤」，不改发布顺序 |
| IF-M4（`before_react_start` 启动闸门） | builtin 的启动依赖语义直接落在既有闸门内，本批次**不**新增 hook |

**被扩展的接口（须在 §5 登记后才能改）**：

| 扩展 | 内容 |
| --- | --- |
| `TransportConfig` 的变体集合 | 新增 `Builtin`（batches 间的第一个新变体；未来 Cron / LSP / Workspace 会再加实例，但**不**应再加变体） |
| `ConfigSource` 的变体集合 | 新增 `Builtin { instance }`，首次把「运行时注入来源」写进该枚举 |
| `MIDDLEWARE_NAMES` 的语义 | **A7 后不再承载策略键**：只含链槽位名；新增 `BUILTIN_INSTANCE_POLICY_KEYS` 承载 builtin 实例关闭键（两表并集 = 迁移前的已知键全集） |
| `default_requires_approval` / `is_edit_tool` / `is_mutation_tool` 的判定面 | 首次出现「按 effective name 归一后判定」的规则（IF-D6/IF-D15，A4）；未知 `mcp__*` 的保守语义不变 |
| `McpToolBridge` 的接口面 | 新增（冻结）：builtin 来源工具的 `prompt_declaration()` 返回值（A9）；声明 direct 在类型化构造点的生效（IF-D13，A5） |
| builtin 实例的 `call_tool` 结果形态 | 首次冻结 `CallToolResponse` 的成功 / 失败映射（IF-D14，A15） |
| builtin 实例名（保留名） | 首次出现「保留实例名」概念（`web`/`artifact` + 预留 `cron`/`lsp`/`workspace`），用户不得用 `command`/`url` 接管（A3） |
| `McpTaskKey` | 本批次**不**扩展（如需扩展须登记 R11） |

**与 acceptance 的关系**：本批次把该记录的**契约 6 BLOCKED 项**转为正面闭合（§8 第 2/3 行 + V-06 的 host 夹具复证），并把契约 5 的**实例落地**从「未落地」推进到「两个 builtin 实例落地」；契约 5 的 `UNVERIFIED` 子项（凭据隔离、capability root 隔离）在本批次**仍不承诺**，且 A13 已冻结其在 builtin 形态下**不可证伪**，acceptance 必须继续标注并按「未验证项清单」模板列出（含 `PERI_MCP_BUILTIN=off` 的运维语义）。

## 12. sub-plan 索引

> 本批次共登记 4 份 sub-plan，四份均已产出（E 与主计划同批次交付；F / G / H 为并行编写）。**主计划是唯一裁决**：§5 的覆盖登记（R1–R33）优先于任何 sub-plan 的表述；子计划保留其领域内的细节权威。四份子计划头部均有一段「§0 裁决记录 A1–A19 落点」，用于逐条复核。

| sub-plan | 文件 | 职责 | owner task 前缀 | 状态 |
| --- | --- | --- | --- | --- |
| **E：Builtin MCP 运行时** | [`2026-09-26-mcp-adaptation-v4-part-2-sub-plan-e-builtin-runtime.md`](2026-09-26-mcp-adaptation-v4-part-2-sub-plan-e-builtin-runtime.md) | builtin 注册表、`TransportConfig::Builtin`、同进程 spawn / 生命周期 / 关闭 / reconnect、默认配置层注入与覆盖语义（step 6.5）、`transport_type`、超时与失败路径、与既有测试的冲突处置 | `E-` | **已产出**（受 §5 R1/R2/R3/R6/R10/R11/R12/R13/R14/R16/R18/R20/R22/R30/R31 覆盖） |
| **F：Web / Artifact 实例与装配** | [`2026-09-26-mcp-adaptation-v4-part-2-sub-plan-f-instances-web-artifact.md`](2026-09-26-mcp-adaptation-v4-part-2-sub-plan-f-instances-web-artifact.md) | 两个 builtin server 的实现与工具核心操作抽取、IF-D14 结果映射、`system_mcp_tools` 声明、默认层内容、链与 4 个挂载点删除、`ChainSlot`/blueprint 与装配测试期望值同步、A6 的三个工具面、A9 的声明段保留 | `I-` | **已产出（并行编写）**，受 §5 R13–R18/R20–R27/R29–R31 仲裁 |
| **G：策略一致性与投影** | [`2026-09-26-mcp-adaptation-v4-part-2-sub-plan-g-policy-parity.md`](2026-09-26-mcp-adaptation-v4-part-2-sub-plan-g-policy-parity.md) | 归一 helper 的 7 个消费点、`sensitive_tool_entries` 条目名与条文、能力关闭（IF-D10）的四个面、静态表与跨 crate 投影（meta_harness / provider config / tool_projection / TOOL_PARAM_ALIASES / hooks / tool_catalog / stage_builder 谓词）、TUI（S-08）与文本面（S-03/S-04/S-05/S-06） | `S-` | **已产出（并行编写）**，受 §5 R5/R15/R17/R21/R24/R25/R26/R32/R33 仲裁 |
| **H：验证与文档** | [`2026-09-26-mcp-adaptation-v4-part-2-sub-plan-h-verification.md`](2026-09-26-mcp-adaptation-v4-part-2-sub-plan-h-verification.md) | crate 内启动/审批/关闭矩阵/隔离可观察断言、host seam 首个 LLM 请求与 fatal 路径、迁移前基线（V-06）、wire 夹具、隔离与宿主策略回归复跑、acceptance 记录、code-index 与标准口径同步 | `V-` | **已产出（并行编写）**，受 §5 R19/R27/R28/R29/R32 仲裁 |
