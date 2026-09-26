# MCP adaptation v4-part-2 — sub-plan G：策略一致性与投影同步

> **主 plan 覆盖（优先于本文）**：主计划 [`2026-09-26-mcp-adaptation-v4-part-2-plan.md`](2026-09-26-mcp-adaptation-v4-part-2-plan.md) 的 §5 覆盖登记优先于本文任何表述。涉及本文的覆盖如下（**实施前必读**）：
>
> | 覆盖 | 内容 | 本文受影响处 |
> | --- | --- | --- |
> | **R4 / A19** | `sensitive_tool_entries()` **保持 14 项 / 3 前缀**，但：① `mcp__` 前缀条目的 **`description` 文本必须改写**为与 parity 判定一致；② **A19**：已迁移条目（`WebFetch`/`WebSearch`）的 `name` 改为 **effective name**（`mcp__web__WebFetch` / `mcp__web__WebSearch`，顺序不变），一致性测试的探测名随之更新；**禁止**新增 builtin 例外条目（会破 `permission/mod_test.rs:472-482` 的计数断言） | **IF-G3 重写**：计数与顺序不变、`mcp__` description 必改、两条目 `name` 改为 effective name；`10_hitl` 渲染列表的断言形态随之改为「含两个 web effective name、不含裸名、不含 artifact」 |
> | **R6 / A6** | 关闭过滤必须落在 IF-D10 的**四个面**（启动提交的 required 集合 / `mcp/middleware.rs` 的 deferred bridge / `preparation.rs` 的 parent_tools / **workflow agent 工具面**——A6 后 workflow **不再**「天然不接入」），且以注册表 `policy_key` 为**唯一**映射；**禁止**按字符串硬编码 `mcp__web__` 前缀 | 本文新增 §6.10；`closed_instances` / `is_closed` 的实现归 **E-02**（`mcp/builtin/mod.rs`），过滤落点归 **E-03 / I-03**；G 只提供判定语义与验收行 |
> | **R17** | 本文的 S-01 可选分支 **O1**（把 `artifact` 加进敏感清单）**缺省不启用**；parity 是唯一缺省语义 | **IF-G2 第 1 条的 O1 保持为「需主 plan 显式裁决才启用」**，不得默认启用 |
> | **R18** | 任务编号仲裁（与 F 共用规则）：执行以主 plan §6 为准 | 见下表「任务映射」 |
> | 主 plan §4（**A8 已裁决**） | `peri-tui/**` **属于本批次**：S-08 持有 `kit/tool_display.rs`、`truncate.rs`（+ 各自测试） | **A8 采用 A4 的归一方案（不是硬编码 effective name）**：上述两处按名分支走 `original_tool_name_of_effective`（匹配型：原样优先、未命中再用原始名）。不改的后果 = 工具卡片无参数摘要、WebFetch 输出走通用截断、`SearchExtraTools` 摘要退回通用 JSON。`kit/tool_semantics.rs`、`kit/acp_types/current_turn.rs` 经核查**无** Web/artifact 按名分支（不列改动） |
>
> **任务映射（G → 主 plan §6）**：
>
> | 本文 task | 主 plan task | 说明 |
> | --- | --- | --- |
> | S-01（三函数 parity + 条目文本） | **S-01** | 同名同义，主 plan 已合并 `permission/*` 与 `subagent/*` 到单一 S-01（本文拆成 S-01 + S-02 是为了给出断言细节，派工时按主 plan 合成一个 task，或按主 plan §4 的文件归属分两次提交） |
> | S-02（subagent mutation + 守护 + agent 断言） | **S-01** | 同上；`subagent/mod.rs` + `mod_test.rs` 在主 plan §4 归 S-01 |
> | S-07（`ToolKind` 投影） | **S-02** | `peri-acp/src/event/tool_projection.rs` + `mapper_test.rs` |
> | S-09（`TOOL_PARAM_ALIASES`） | **S-02** | `peri-agent/src/tools/invocation.rs` + `invocation_test.rs` |
> | S-03（`web-researcher.md`）、S-05（`use-artifacts/SKILL.md`）、S-06（prompt 段落）、S-08（TUI）、S-10（`docs/reference/mcp-ecosystem.md`） | **已由主 plan 登记（A19）**：S-03 → W3；S-05 → W1；S-06 → W1；S-08 → W3（TUI，A8）；S-10 → **主 plan V-05（W5）** | 这五项是 G 的增量，主 plan §4/§6/§7 已给出 owner 与波次；G 侧只保留断言细节权威 |
>
> **范围纪律（硬要求，主 plan §9 规则 4 追加）**：本波**只**改「被迁移的两个实例的工具名」（`WebSearch` / `WebFetch` / `artifact`）。**其余工具名一个字都不改**。凡本文件判定为「不属本波」的文件，见到名字相同也**不得**顺手改。

## 0. 裁决记录 A1–A19 落点（复核用索引）

| 裁决 | 本文件落点 |
| --- | --- |
| **A1** 注入点唯一（step 6.5） | 不涉及（归 I-02/E；G 不写 loader） |
| **A2** `PERI_MCP_BUILTIN` 语义 | §6.10（关闭的三条路径之一：off ⇒ 两实例都不注入、能力面为零，不是回退到旧实现） |
| **A3** 保留实例名 | **IF-G2 末条新增反例**（外部 server 名 `artifact` + 工具 `artifact` ⇒ 要么加载失败、要么 `default_requires_approval("mcp__artifact__artifact") == true`）；§6.1；§7.1 S-01 断言 6 |
| **A4** 生效名归一原则 | **IF-G1 全文重写**（唯一 helper 落 `peri-acp-types`；7 个消费点全集；判定型 = 替换、匹配型 = 原样优先；事件载荷与 wire 仍为 effective name）；IF-G3 / IF-G5 / §6.5 / §6.6 / §6.7 全部改为该原则的实现 |
| **A5** 直连性声明（IF-D13） | 不涉及（归 E-01/E-03 与 I-03）；G 只在 §6.10 引用关闭集的来源 |
| **A6** 三个工具面 | §6.10（四个面，workflow 不再「天然关闭」）；头部 R6 行 |
| **A7** 两张名单 | §7.1 S-01/S-02 断言（`BUILTIN_INSTANCE_POLICY_KEYS` + 集合相等改强度不降的新形态）；§4（`provider/config.rs` → S-02）；§12 |
| **A8** TUI 在范围内 | **§6.6 重写**（走归一，**不得**硬编码 `mcp__web__*`）；头部「主 plan §4」行；§4 TUI 行；§7 表 S-08 行（W3）；§8；§9 |
| **A9** 声明段不得丢失 | §6.5（声明段投影与 `ToolKind` 同层的处置：声明归 **S-02**，G 只声明「不得丢失」并给验收行）；§10 |
| **A10** V-03 夹具 | 不涉及（归 V-02/V-06） |
| **A11** 隔离夹具 env | §9（隔离证据引用 V-03 的复跑；夹具 env 改动由 V-03 承担） |
| **A12** 基线证据 | §9（「首请求工具名」类断言引用 V-06 基线；G 不持 host 文件） |
| **A13** 隔离断言 | 不涉及（归 V-01/V-03） |
| **A14** 任务号与过滤器 | §7 表全部命令；§7.1 断言名；旧装配编号统一读作 `I-03` |
| **A15** `call_tool` 映射（IF-D14） | 不涉及（归 I-01） |
| **A16** duplex 容量 | 不涉及（归 E-03） |
| **A17** `{"web": {}}` 语义 | §6.10（关闭/启用的配置语义）；§12 |
| **A18** 关闭片段形状 | §6.10（唯一合法关闭片段 + 非法组合必须被加载期拒绝，反例用例归 I-02）；§10 G-R6 |
| **A19** 主计划唯一裁决源 | 头部任务映射（S-03/S-04/S-05/S-06/S-08/S-10 的 owner 与波次）；**IF-G3（条目 `name` 改 effective name）**；§4；§7 表；§11 |

## 1. 元信息

- 日期：2026-09-26。状态：实现规划，**未实施**。本轮只写本文，不写生产代码、不改设计文档、不提交。
- 范围：wave 1 = Web MCP / Artifact MCP 迁移所引发的策略判定与投影面同步。
- 交付判据（本文件的可验证产物）：
  1. 对 `mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`，`default_requires_approval` / `is_edit_tool` / `is_mutation_tool` 的判定**等于**对原始名的判定（IF-D6），且每个改动都有**反证测试**（未知 `mcp__*` 保守语义分毫不变）与**等价性测试**；
  2. `sensitive_tool_entries()` 与 `10_hitl` 段落同步且一致性锁定测试仍绿；
  3. 每一个会被工具名影响的投影面，都有明确处置 + 「不改会怎样」的具体后果 + 承担断言。
- 本轮**不**宣称：迁移整体完成；TUI 对 MCP 工具的展示等价；hook 配置自动迁移。

## 2. 事实基线（已核实）

### 2.1 策略判定面

| 事实 | 位置 |
| --- | --- |
| `default_requires_approval(tool_name)` 现为：`Bash`/`folder_operations`/`Agent`/`RunPtcCode`/`Write`/`Edit`/`delete_*`/`rm_*`/`WebFetch`/`WebSearch`/`mcp__*`/`DynamicMCP.load`/`DynamicMCP.unload`/`cron_register` | `peri-middlewares/src/permission/mod.rs:44-59`（`mcp__` 前缀在 `:55`） |
| **`artifact` 不在该清单里** → 当前 `artifact` 工具**不需审批** | 同上（清单逐项已核对） |
| `is_edit_tool(tool_name)` 只认 `Write`/`Edit`/`folder_operations` | `peri-middlewares/src/permission/mod.rs:65-67` |
| `is_mutation_tool(name)` = `bash|write|edit|folder_operations|cron_register`（小写化精确）+ `mcp__*` 前缀无条件 `true` | `peri-middlewares/src/subagent/mod.rs:389-395` |
| `core_mutation_tools_fully_disallowed(disallowed)` 的 `MUTATION_CORE = [bash, write, edit, folder_operations, cron_register]` | `peri-middlewares/src/subagent/mod.rs:402-414` |
| 消费点：`infer_agent_capability` → `AgentCapability.can_mutate` → prompt 渲染 `readonly`/`writes` | `peri-middlewares/src/subagent/mod.rs:430-449`；`peri-acp/src/prompt/mod.rs:445` |
| 既有断言「`mcp__*` 无法证明只读，应保守标 writes」 | `peri-middlewares/src/subagent/mod_test.rs:495-500` |
| 工具名常量源（14 个 Core 名 + `TOOL_WEBFETCH`/`TOOL_WEBSEARCH`） | `peri-middlewares/src/tool_search/core_tools.rs:16-29` |
| `sensitive_tool_entries()` 返回 `[SensitiveToolEntry; 14]`，是 `10_hitl` 动态段落的**单一事实源** | `peri-middlewares/src/permission/mod.rs:91-164` |
| 一致性锁定测试（条目 ↔ `default_requires_approval` 一一对应）与条目计数断言（14 项 / 前缀 3 项） | `peri-middlewares/src/permission/mod_test.rs:436-482` |
| `10_hitl` 段落渲染由持有者 middleware 装配，渲染内容含 `format_sensitive_tools()` 输出 | `peri-middlewares/src/permission/mod.rs:215-229`、`:167-179` |
| 段落渲染锁定测试（含 `` `Bash` — shell command execution `` 逐字断言） | `peri-acp/src/prompt/prompt_test.rs:225-247`（断言 :243-246） |
| `10_hitl.md` 静态部分只在 `:8` 点名 `Write`/`Edit`/`folder_operations`（AcceptEdit 语义），**不含** Web/Artifact | `peri-acp/prompts/sections/10_hitl.md:8` |
| `TOOL_PARAM_ALIASES` 用 `target.name().eq_ignore_ascii_case("WebSearch")` 匹配，含 `("WebSearch","search_term","query")` | `peri-agent/src/tools/invocation.rs:120-125`、`:167-171` |
| `McpToolBridge` **不**覆写 `aliases()` → 回退 trait 默认空切片 | `peri-acp-types/src/tools.rs:585-589`；`peri-middlewares/src/mcp/tool_bridge.rs:212-240`（无 `aliases` 实现） |
| `resolve_target` 匹配 `key` / `tool.name()` / 二者的大小写无关形式 / `aliases`；多命中 → `ambiguous tool invocation` | `peri-agent/src/tools/invocation.rs:79-105` |
| `ToolFilterPolicy::canonical` 对 allowed/disallowed **小写化后精确比较**，不经别名、不支持通配（只支持字面 `"*"`） | `peri-agent/src/session/tool_catalog.rs:118-149` |
| hooks matcher：管道分隔精确 / 纯字母数字下划线精确 / 否则正则；**大小写敏感** | `peri-middlewares/src/hooks/matcher.rs:10-26` |
| hooks `if` 条件式：`parse_permission_rule("Bash(git commit)")` → 工具名**精确相等**比较 | `peri-middlewares/src/hooks/matcher.rs:32-52` |

### 2.2 投影面（逐面核实）

| 投影面 | 现状 | 是否含迁移对象的名字 |
| --- | --- | --- |
| `peri-acp/prompts/sections/01_intro.md:4` | URL 引用纪律，点名 `WebFetch`/`WebSearch` | **是** |
| `peri-acp/prompts/sections/05_using_tools.md:9`、`:13` | 只点名 `Bash` | **否**（已逐行核实，全文件无 Web/Artifact 名） |
| `peri-acp/prompts/sections/07_runtime.md:9` | 只点名 `Bash` | **否** |
| `peri-acp/prompts/sections/10_hitl.md:8` | 只点名 `Write`/`Edit`/`folder_operations` | **否** |
| `peri-acp/prompts/sections/11_subagent.md:15` | 括号枚举 `Bash, Write, Edit, WebFetch, MCP, ...` | **是**（仅 `WebFetch`） |
| `peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md:4`、`:41`、`:46-47`、`:54`、`:113-115` | 教「`artifact` 是 deferred 工具，两步调用」+ 写死 `ExecuteExtraTool({ tool_name: "artifact", ... })` | **是（功能级，非文案级）** |
| `peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md` | 只在泛指意义上出现 `artifact` 一词（「artifacts」作为名词），**不是**工具名 | **否**（核实：`:70-72`、`:139`、`:154` 是泛指用法） |
| `peri-middlewares/src/subagent/built-in/web-researcher.md:4` | 前端 matter `tools: WebFetch, WebSearch, Bash, Write, Read, TodoWrite` + `disallowedTools` 4 项 | **是（功能级：名单裁剪）** |
| `web-researcher.md:18`、`:24-29`、`:39-70`、`:86-95`、`:135-137` | 正文大量点名 `WebSearch`/`WebFetch` | **是（文案级）** |
| `peri-middlewares/src/subagent/built-in/explorer.md:4-11`、`plan.md:10` | `disallowedTools` 列 `Agent/Write/Edit/Bash/folder_operations/cron_register` | **否** |
| `peri-middlewares/src/subagent/built-in/coder.md:4` | `tools: Read, Grep, Glob, Bash, Edit, Write, TodoWrite` | **否** |
| `peri-acp/src/event/tool_projection.rs:62-68` | 工具名 → `ToolKind`（`WebFetch`/`WebSearch` → `Fetch`，未命中 → `Other`） | **是** |
| `peri-tui/src/kit/tool_display.rs:53-62`、`:79-83` | `format_tool_args` 按名取摘要字段（`WebSearch`→`query`、`WebFetch`→`url`、`artifact`→`file_path`） | **是** |
| `peri-tui/src/truncate.rs:186-192`、`:227` | `summarize_input` 同上一组名字 | **是** |
| `peri-tui/src/truncate.rs:275-285` | 输出折叠按名（`WebFetch`） | **是** |
| `peri-tui/src/kit/tool_semantics.rs:58-62` | `presentation_for` 只认 `Skill`/`SkillTool`/`TodoWrite`，其余 `Generic` | **否** |
| `peri-tui/src/kit/acp_types/tool_card.rs:65`、`:83` | 只认 `Edit`/`Write` | **否** |
| `peri-tui/src/kit/acp_types/current_turn.rs:262` | 只认 `Bash` | **否** |
| `peri-tui/src/kit/message_area/render/tool_card.rs:53-58` | 按 `TuiToolPresentation` 分派，**不**读原始工具名 | **否**（其「未命中落 Generic」由 `tool_semantics.rs` 承担） |
| `peri-tui/src/kit/tool_display_test.rs:18-19`、`:23`、`:52-87`、`truncate_test.rs:112` | 上述名字的锁定测试 | **是（随实现改）** |
| `peri-tui/src/cli_integration_test.rs:325-328` | `--disallowed-tools WebFetch` 的 CLI 解析 fixture（**只测解析**，不断言工具存在） | **否**（保持不动，见 §6.7） |
| `peri-middlewares/src/tool_search/declaration_test.rs` | 真实 direct 工具集声明段（含 `WebFetch`/`WebSearch`/`artifact`） | **是**（主 plan **S-02** 拥有；G 不持有） |
| `example/minimal/.peri/settings.json:26`、`:37`、`example/minimal/README.md:53`、`:63` | 遗留 MetaHarness 关闭键 | **是**（A7 后两键迁到 `BUILTIN_INSTANCE_POLICY_KEYS`，仍是**合法键**，因此该 example 继续有效；本波**不改**该 example） |
| `docs/design/meta-harness.md:48`、`docs/meta-harness.md:34`、`docs/design/middleware-system.md:128`、`docs/design/tui-chat-workbench.md:108`、`docs/design/tool-system.md:75` | 设计文档中提及 `WebMiddleware` / `WebFetch` | **是**，但本波无 owner（§8 非目标） |

## 3. 接口冻结

### IF-G1：生效名归一原则（**A4 的唯一 parity 机制**，落地 IF-D6 + IF-D15）

**唯一 helper（落 `peri-acp-types/src/builtin_mcp.rs`，owner E-01）**：

```rust
/// builtin 一等工具的 effective name → 其原始工具名。纯**查表**（冻结字面量），
/// 禁止反拆 `mcp__` 名字、禁止在本 crate 复刻 sanitize 规则；未命中返回 None。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str>;
```

- 表的数据来源 = IF-D5 的三个冻结字面量（`mcp__web__WebSearch` / `mcp__web__WebFetch` / `mcp__artifact__artifact`）；它与 `mcp::builtin::effective_tool_name()`（E-02，内部调用 `tool_bridge::sanitize_name_component`）的**一致性**由 E-02 的锁死测试保证（规则一份实现、字面量一份声明）。
- 匹配为**精确字符串相等**；不得大小写折叠、不得 `strip_prefix("mcp__")` 后再猜。
- 未命中（未知 / 外部 `mcp__*`）⇒ `None` ⇒ 消费点沿用既有保守语义。

**七类消费点全集（冻结；不得新增第 8 处之外的临时特判）**：

| # | 消费点 | 语义 | owner |
| --- | --- | --- | --- |
| ① | `permission::default_requires_approval`（`permission/mod.rs:44-59`） | **判定型 = 替换**：命中则对原始名走现有函数体 | **S-01** |
| ② | `permission::is_edit_tool`（`:65-67`） | 判定型 = 替换 | **S-01** |
| ③ | `subagent::is_mutation_tool`（`subagent/mod.rs:389-395`） | 判定型 = 替换 | **S-01** |
| ④ | TUI 按名分支：`peri-tui/src/kit/tool_display.rs:53`、`:58`；`truncate.rs:186`、`:190`、`:275` | **匹配型 = 原样优先、未命中再用原始名**（A8） | **S-08** |
| ⑤ | `peri-agent/src/tools/invocation.rs` 的 `TOOL_PARAM_ALIASES`（表 `:120-125`，匹配点 `:167-171`） | 匹配型（先按 `target.name()` 原样查表，未命中再用归一后的原始名） | **S-02** |
| ⑥ | `peri-middlewares/src/hooks/matcher.rs` 的 `matches_matcher`（`:10`）/ `matches_if_condition`（`:32`） | 匹配型（matcher 模式侧与 tool_name 侧都按「原样优先」） | **S-01** |
| ⑦ | `peri-agent/src/session/tool_catalog.rs::ToolFilterPolicy::canonical`（`:119`）的 allow/deny 过滤 | 匹配型 | **S-02** |

**判定函数的改写方式（冻结，判定型）**：

```rust
pub fn default_requires_approval(tool_name: &str) -> bool {
    match original_tool_name_of_effective(tool_name) {
        Some(original) => default_requires_approval_without_builtin(original),
        None => default_requires_approval_without_builtin(tool_name),
    }
}
```

其中 `default_requires_approval_without_builtin` 是现有**函数体逐字位移**（含 `tool_name.starts_with("mcp__")` 这一行）。`is_edit_tool` 与 `is_mutation_tool` 同构。

**为什么是「重定向到原始名」而不是「并集/或运算」**：IF-D6 的字面语义是**判定相等**。并集会让 `mcp__web__WebSearch` 因 `mcp__` 前缀继续算 mutation（与原始名 `WebSearch` 的判定不等），直接违反 IF-D6。代价是**判定结果会变**，必须逐条显式记录（§3.2 IF-G2）。

**冻结约束（全部必须写进测试或文档）**：
1. **禁止按名硬编码**：任何消费点都**不得**出现 `mcp__web__WebFetch` 之类的字面量分支、`starts_with("mcp__web__")`，也不得自建第二张反查表（与声明表漂移即失去 parity）。
2. **事件载荷与 wire 仍暴露 effective name**：归一**只**用于判定与匹配，不得改写 `ToolCall` 载荷、ACP 事件、transcript 或 MCP wire 上的名字（投影真值不得被污染）。
3. **未知 / 外部 `mcp__*` 的保守语义分毫不变**（反证测试，见 §7.1）。
4. **保留名反例（A3）**：外部 server 名 `artifact` + 工具 `artifact`（或 server 名 `web` + 工具 `WebSearch`）⇒ 要么**加载期失败**（保留名 typed error，A3），要么 `default_requires_approval("mcp__artifact__artifact") == true`（即不被当作 builtin 一等工具放行）。两者之一必须成立——这是「parity 不得只按名字反查」的证据。
5. 本 helper 是**唯一**归一入口；`subagent/mod.rs` 经 `peri_acp_types::builtin_mcp::original_tool_name_of_effective` 调用（不跨 crate 复制逻辑）。

### IF-G2：三个冻结名字的判定结果表（**必须由测试逐项锁定**）

把 IF-G1（判定型 = 替换语义）代入现有清单，wave 1 的冻结结果如下（`=` 右侧是对原始名的判定，也是迁移后 effective name 的判定）：

| effective name | 原始名 | `default_requires_approval` | `is_edit_tool` | `is_mutation_tool` | 与迁移前的差别 |
| --- | --- | --- | --- | --- | --- |
| `mcp__web__WebSearch` | `WebSearch` | `true`（`TOOL_WEBSEARCH` 在清单内） | `false` | **`false`**（`"websearch"` 不在精确集合） | 审批不变；mutation 由「`mcp__` → true」变为 `false` |
| `mcp__web__WebFetch` | `WebFetch` | `true`（`TOOL_WEBFETCH` 在清单内） | `false` | **`false`** | 同上 |
| `mcp__artifact__artifact` | `artifact` | **`false`**（`artifact` 不在清单内） | `false` | **`false`** | 审批由「`mcp__` → true」变为 `false`；mutation 同上 |

**必须显式记录的决策（不得靠读者推断）**：

1. `mcp__artifact__artifact` 变成**不需审批**，是「与迁移前的 `artifact` 工具行为等价」的结果，**不是**新的放行。依据：`artifact` 工具今天的 effective name 就是裸名 `artifact`（`peri-middlewares/src/artifact/tool.rs:105-107`），而 `artifact` 不在 `default_requires_approval` 清单（`permission/mod.rs:44-59`）。风险定性：Artifact 上传会把内容发布到**公开 URL**，属于「今天已存在、不因本波新增」的风险面。
   **若产品判定迁移后应当收紧**（即 artifact 也需要审批），那是**独立的策略变更**，不得由本波顺带完成：需要 F-IF-F6 的 `default_requires_approval` 清单新增 `"artifact"` 条目 + `sensitive_tool_entries()` 增项（14 → 15）+ 段落与计数测试同步，并在 acceptance 里单列为契约变更。本文件把该选项登记为 **S-01 的可选分支 O1**，由主 plan 裁决；**缺省不启用**。
2. 两个 Web 工具的 mutation 判定由 `true` 变为 `false`，会让「`tools:` 名单只含 Web 工具的 agent」从 `writes` 变 `readonly`（经 `infer_agent_capability`）。这是**方向正确的收紧之外的方向**（更少的假 writes），但会改变 `11_subagent` 段落的 catalog 文案（`peri-acp/src/prompt/mod.rs:445` 渲染）。需要一条断言钉住 `web-researcher` 的最终标签（§6.3）。

**反证（不得省略）**：未知 `mcp__*` 必须**分毫不变**地保守。既有断言为：

- `peri-middlewares/src/permission/mod_test.rs:97-102`（`mcp__filesystem__read_file` / `mcp__filesystem__write_file` / `mcp__github__create_issue` / `mcp__database__query` / `mcp__web__fetch` 全部 `true`）
- `peri-middlewares/src/permission/mod_test.rs:123-134`（`test_mcp_prefix_edge_cases`：`mcp_` / `mcp` 不匹配，`mcp__a__b` / `mcp__x__y__z` 匹配）
- `peri-middlewares/src/subagent/mod_test.rs:495-500`（`tools: [Read, mcp__files]` → `can_mutate == true`）

这三条是**反证基线**：它们必须在 S-01/S-02 之后**原样通过**，且**不得**为了新行为改写其断言。特别注意 `mcp__web__fetch`（小写 `fetch`）与 `mcp__web__WebFetch` **不是**同一个名字——前者走未知路径（true），后者走 builtin 映射（`WebFetch` → true）。两者恰好都 true，但**理由不同**，因此必须各有一条用例，且后者必须断言「命中了 builtin 映射」而不是「撞上了前缀」。

### IF-G3：`sensitive_tool_entries()` 与 `10_hitl` 段落的同步规则

`SensitiveToolEntry` 的文档契约（`permission/mod.rs:71-76`）是「与 `default_requires_approval` 的判定分支**一一对应**」，且落在 `mcp__` 前缀条目上（`permission/mod.rs:143-147`：`name: "mcp__"`, `prefix_match: true`）。

**结论（已被主 plan R4 + **A19** 修正，以此为准）**：条目表的**数量与顺序不变**（仍 14 项、前缀仍 3 条，`permission/mod_test.rs:472-482` 的计数断言不得改数字），但有**两处内容变更**：
1. **`mcp__` 前缀条目的 `description` 必须改写**为与 parity 判定一致的文本（例如「any MCP server tool (prefix match); builtin first-class capabilities follow their original tool's rule」）。理由：条目表的文档契约是「与 `default_requires_approval` 的判定分支**一一对应**」，parity 让 `mcp__` 前缀不再是**充分**条件（已知 builtin 的 effective name 走原始名规则），条文必须跟上，否则段落会对模型说错话。
2. **A19：已迁移条目（`WebFetch` / `WebSearch`，`permission/mod.rs:133-142`）的 `name` 改为 effective name** —— `mcp__web__WebFetch` / `mcp__web__WebSearch`，顺序不变，`prefix_match` 仍为 `false`。理由：迁移后模型面（与 TUI/段落）看到的名字就是 effective name，条目名若保留裸名，渲染出的 `10_hitl` 列表会**点名一个已不存在的工具**。`TOOL_WEBFETCH` / `TOOL_WEBSEARCH` 常量本身不改（`tool_search/core_tools.rs:24-25` 仍有其它调用点）。

**因此 S-01 的断言**：
- 既有 `sensitive_entries_match_default_requires_approval`（`permission/mod_test.rs:436-482`）**必须原样通过、不得改数字**——该测试只探测 `entry.name`（前缀条目探测 `"{name}some_tool"`，即 `mcp__some_tool`，属**未知** builtin → 仍敏感）；S-01 只需**更新两个相异探测名**（探针从裸名改为 effective name，语义不变），计数仍为 14 / 3。
- 新增断言：`mcp__` 条目的 `description` **不再**宣称「所有 `mcp__*` 一律敏感」；渲染出的 `10_hitl` 段落文本**含** `mcp__web__WebFetch` / `mcp__web__WebSearch`，**不含**裸名 `WebFetch` / `WebSearch` / `artifact`，**不含** `mcp__artifact__artifact`（artifact 不在敏感清单）；`prompt_test.rs:225-247` 的既有逐字断言（针对 `Bash` 条目）不改。
- 若 S-01 的实现导致计数断言失败，说明实现偏离了 IF-G1（很可能是把 `mcp__` 前缀分支改成了「枚举 builtin 例外」），必须回退实现而不是改测试。

**可选分支 O1 的影响面**（仅在主 plan 裁决启用时执行）：条目表 14 → 15（新增 `artifact`，`prefix_match: false`，`description` 需与 F 一致）、`mod_test.rs` 的计数从 14 改 15、`prompt_test.rs:243-246` 的逐字断言可能需增补、acceptance 记录该策略变更。**缺省不启用**（R17）。

### IF-G4：`subagent/built-in/*.md` 与 `core_mutation_tools_fully_disallowed` 的处置

- `core_mutation_tools_fully_disallowed` 与 `MUTATION_CORE`（`[bash, write, edit, folder_operations, cron_register]`）：**本波不改**。依据：Wave 1 的迁移对象不在该集合内；`mcp__*` 无法用精确 disallowed 排除是该函数的**已知局限**，已写在 `subagent/mod.rs:397-401` 的注释里，本波不扩大其范围。**追加一条守护测试**（S-02）：断言 `MUTATION_CORE` 与 `BUILTIN_MCP_INSTANCES` 的工具名集合**不相交**——一旦将来把 Web/Artifact 之外的实例（例如假想的 Workspace 实例的 `Write`）纳入 builtin 表，该测试会立刻提醒 owner 复核这个「无法精确排除」的盲区。
- `explorer.md` / `plan.md` / `coder.md` / `general-purpose.md` / `verification.md`：**本波不改**（已核实其 frontmatter 不含迁移对象名）。
- `web-researcher.md`：**必须改**，且必须区分两类改动（差别的性质不同，不得混为一个 task 的一句话）：
  - **功能级（必改，否则能力丢失）**：`tools:` 前端 matter 里的 `WebFetch, WebSearch` → `mcp__web__WebFetch, mcp__web__WebSearch`。依据：`ToolFilterPolicy::canonical` 小写化后**精确**比较（`tool_catalog.rs:118-149`），`websearch` ≠ `mcp__web__websearch`。不改的后果：该内置 agent 的白名单**同时**排除了旧的 `WebSearch`（已不存在）与新的 `mcp__web__WebSearch`（名字不符）→ 子 agent 一个 Web 工具都没有，`web-researcher` 变成空壳。
  - **文案级（应改，不改不破坏执行）**：正文中的 `` `WebSearch` `` / `` `WebFetch` `` 字样。不改的后果：模型在该 agent 内看到工具列表里是 `mcp__web__WebFetch`，而正文教它用 `WebFetch`；若它照正文发出裸名，`resolve_target` 找不到（`McpToolBridge::aliases()` 为空，`invocation.rs:79-105`）→ `AgentError::ToolNotFound`。属「可靠但不必然触发」的风险，因此**同一 task 内一并改**，不留悬空。
- `disallowedTools` 的 4 项（`Edit`/`Glob`/`Grep`/`folder_operations`）**不改**——它们不是迁移对象。

### IF-G5：`TOOL_PARAM_ALIASES` 的处置（必须改，否则参数归一化静默失效）

`peri-agent/src/tools/invocation.rs:120-125` 的 `("WebSearch", "search_term", "query")` 靠 `target.name().eq_ignore_ascii_case("WebSearch")` 生效（`:167-171`）。迁移后 `target.name()` 是 `mcp__web__WebSearch` → **不匹配** → 别名不再生效。

**后果（具体）**：模型若输出 `{"search_term": "..."}`（该别名的存在本身说明模型会这么输出），`apply_param_alias` 不执行 → 输入里没有 `query` → MCP server 侧 schema 校验拒绝该调用（`required: ["query"]`，见 `web_search.rs` 的 `parameters()`）→ 工具调用失败。这是**静默**的：没有编译错误、没有测试红（因为既有别名测试用的是裸名 fixture，`invocation_test.rs`）。

**冻结处置（S-02，**A4 ⑤ 匹配型归一**；不再有「选项 A/B」二选一）**：

在匹配点（`invocation.rs:167-171`）把 `target.name()` 按**匹配型**规则处理：**先按原样查表**（保证用户/既有裸名行继续生效），**未命中再取 `peri_acp_types::builtin_mcp::original_tool_name_of_effective(target.name())`** 后重查一次。这样：
- `mcp__web__WebSearch` 会命中 `("WebSearch","search_term","query")` 行（经归一）；
- 若将来引入「effective name 直接成行」的写法也不冲突，但**不得**新增硬编码 `mcp__web__*` 行（A4 明确禁止按名硬编码 effective name）；
- 不需要在 `peri-agent` 侧复刻 effective name 模板（旧「选项 B」的算法重复问题消失，因为它只是查表）。

**必须附的锁定测试**（S-02）：
1. 对 `BUILTIN_MCP_INSTANCES` 的每个 `(instance, tool)`，断言「若某工具名需要参数别名，则该 effective name 与裸名**都能**命中该别名」（当前只有 `WebSearch` 需要，故断言集合 = {`WebSearch`, `mcp__web__WebSearch`}）。
2. **反证**：非声明表的工具名（如 `Read`）只匹配裸名；`("Write","contents","content")` 与 `("Glob",…)` 的既有两条条目行为**逐位不变**（`invocation_test.rs` 的既有用例不得改写）；未知 `mcp__*`（如 `mcp__some__tool`）不命中任何别名行。
3. 断言别名**不**改变工具解析面：模型发出的工具名仍是 effective name（别名只是 input 字段归一），`resolve_target` 的候选数不因别名增加。
4. 断言实现里**没有** `mcp__web__` 字面量（可 grep 的事实）。

**不允许**的替代方案（写进计划以免 owner 顺手采用）：① 给 `McpToolBridge` 加 `aliases()` 返回裸名（见 F §11 非目标：会引入双名字与 `ambiguous tool invocation`）② 在表里新增 effective name 行（A4 禁止按名硬编码）。

### IF-G6：投影面的处置原则（防止范围蔓延）

1. **功能级**（名单裁剪、参数匹配、策略判定、`ToolKind` 分类、**按名判定/匹配的归一**）→ **必改**，本波完成；且**只能**经 IF-D15 的单一 helper（A4），不得逐点硬编码 effective name。
2. **文案级**（prompt 段落、skill 正文、内置 agent 正文）→ 只改**点名了迁移对象**的位置；不改泛指名词（例如 `ultracode/SKILL.md` 里的「artifacts」是名词，不是工具名）。
3. **不属本波**（名字虽在但语义无关）→ 见 §2.2 的「否」行与 §11 非目标。特别地：`tool_semantics.rs` / `tool_card.rs` / `current_turn.rs` / `message_area/render/tool_card.rs` / `cli_integration_test.rs` **一行都不改**（其中 `tool_semantics.rs`、`current_turn.rs` 已逐行核查：无 Web/artifact 按名分支）。
4. 每一处改动都必须在任务表里给出「不改会怎样」的一句具体后果；写不出具体后果的改动一律不做。
5. **归一不得泄漏到投影真值**：ACP 事件载荷、transcript、MCP wire 上的名字仍是 effective name（A4）。

## 4. 文件所有权矩阵（wave 1 的 G 部分）

| 文件 | owner | 备注 |
| --- | --- | --- |
| `peri-middlewares/src/permission/mod.rs`、`permission/mod_test.rs` | **S-01 唯一** | IF-G1/G2/G3 全在这两个文件；三个判定函数 + 条目表 + 一致性测试**必须同一 task**（同文件多 owner 会立刻冲突） |
| `peri-middlewares/src/subagent/mod.rs`、`subagent/mod_test.rs` | **S-02 唯一** | `is_mutation_tool` 重定向 + `MUTATION_CORE` 守护 + 既有 mcp 断言保持 |
| `peri-middlewares/src/subagent/built-in/web-researcher.md` | S-03 唯一 | 功能级 frontmatter + 文案级正文 |
| `peri-acp/src/prompt/prompt_test.rs` | S-04 唯一 | 段落渲染锁定；与主 plan I-03/S-02 不冲突（不同 crate） |
| `peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md` | S-05 唯一 | deferred → direct 的调用流程改写 |
| `peri-acp/prompts/sections/01_intro.md`、`11_subagent.md` | S-06 唯一 | 只改点名位置；**不得**打开 `05_using_tools.md` / `07_runtime.md` / `10_hitl.md` |
| `peri-acp/src/event/tool_projection.rs`、`event/mapper_test.rs` | S-07 唯一 | `ToolKind` 分类；需带反证（未知名仍 `Other`） |
| `peri-tui/src/kit/tool_display.rs`、`kit/tool_display_test.rs`、`truncate.rs`、`truncate_test.rs` | S-08 唯一 | **A8：TUI 属于本批次**；走 A4 归一（**不得**硬编码 `mcp__web__*`），4 个文件同一 task |
| `peri-middlewares/src/hooks/matcher.rs` | S-01 唯一 | A4 ⑥ 匹配型归一；`peri-middlewares/src/hooks/` 下其它文件不碰 |
| `peri-agent/src/session/tool_catalog.rs` | S-02 唯一 | A4 ⑦ 匹配型归一（`ToolFilterPolicy::canonical`） |
| `peri-agent/src/tools/invocation.rs`、`invocation_test.rs` | S-09 唯一 | IF-G5；`peri-agent/src/tools/` 下的其他文件不碰 |
| `docs/reference/mcp-ecosystem.md` | S-10 唯一 | 用户可见的名称/关闭/hook 说明；**不动** `docs/design/**`、`docs/standards/**`、`docs/code-index/**` |
| `peri-middlewares/src/lib.rs` | **主 plan I-03 唯一** | G **不**触碰（反查函数声明在 `permission/mod.rs`） |
| `peri-middlewares/src/tool_search/declaration_test.rs` | **主 plan S-02** | G 不持有 |
| `example/minimal/**` | **无 owner（本波不改）** | 主 plan R15：两键保留后该 example 继续有效，充当遗留键路径的活体样本 |
| `peri-acp-types/src/builtin_mcp.rs` | **主 plan E-01** | G 只**读** `BUILTIN_MCP_INSTANCES`；effective name 计算函数在 `peri-middlewares/src/mcp/builtin/mod.rs`（**E-02**），G 也只读 |

## 5. 覆盖登记

| 编号 | 覆盖内容 | 以何为准 |
| --- | --- | --- |
| R-G1 | IF-D6 的「判定相等」按**重定向到原始名**实现，而非并集；由此产生的三项判定变化逐条登记并测试锁定 | 本文 IF-G1/G2 |
| R-G2 | `mcp__artifact__artifact` 变为不需审批是「与迁移前裸名行为等价」的结果；若主 plan 判定需收紧，走 S-01 的可选分支 O1（独立策略变更，不由本波默认启用） | 本文 IF-G2 第 1 条 |
| R-G3 | `sensitive_tool_entries()` **不因 IF-G2 变化而增删条目**；既有 14 项 / 前缀 3 项计数断言不得改数字（**A19 只改两个已迁移条目的 `name` 与 `mcp__` 条目的 description**） | 本文 IF-G3 |
| R-G4 | `TOOL_PARAM_ALIASES` 的匹配改为**匹配型归一**（原样优先、未命中再用原始名），且**不得**硬编码 effective name 字符串、不得新增 effective name 行 | 本文 IF-G5 |
| R-G5 | `core_mutation_tools_fully_disallowed` 不改，但需一条 `MUTATION_CORE ∩ builtin 工具名 == ∅` 的守护测试 | 本文 IF-G4 |
| R-G6 | `web-researcher.md` 的 `tools:` frontmatter 是**功能级必改**（不改则该 agent 失去全部 Web 工具）；正文是文案级 | 本文 IF-G4 |
| R-G7 | 判定为「不属本波」的 5 个文件（`tool_semantics.rs` / `acp_types/tool_card.rs` / `acp_types/current_turn.rs` / `message_area/render/tool_card.rs` / `cli_integration_test.rs`）明确不改 | 本文 §2.2 + IF-G6 |
| R-G8 | ACP `ToolKind` 投影（`WebFetch`/`WebSearch` → `Fetch`）不得因改名退化为 `Other` | 本文 §6.5 |
| R-G9 | 用户 hook 配置与 `--disallowed-tools` 使用裸名会失效；本波只做文档声明，不做兼容 | 本文 §6.8 |

## 6. 逐投影面的处置与后果

### 6.1 策略判定（S-01 / S-02）

见 IF-G1 / IF-G2 / IF-G3 / IF-G4。**不改会怎样**：
- 不改 `default_requires_approval`：`mcp__artifact__artifact` 会继续命中 `mcp__` 前缀 → 每次上传都弹审批。这不是安全回归，但**违反 IF-D6**，且与迁移前行为不一致（同一操作在迁移前后要求不同的用户动作）。
- 不改 `is_mutation_tool`：`web-researcher` 及任何 `tools:` 里写 effective name 的 agent 一律被标 `writes` → 在支持并行 agent 的编排里无法并行（`11_subagent` 段落的 `readonly` 提示是调度依据），且与 IF-D6 不等价。

### 6.2 `10_hitl` 段落（S-01 + S-04）

段落是**动态生成**的（`permission/mod.rs:215-229` 拼 `format_sensitive_tools()`），因此条目表不变 → 渲染文本不变。**不改会怎样**：无变化（这是本项**不**产生 diff 的正当理由，不是遗漏）。S-04 的存在价值是**防止 S-01 悄悄改了条目表**：`prompt_test.rs:243-246` 的逐字断言（`` `Bash` — shell command execution ``）与 `mod_test.rs` 的计数断言互为双锁。

### 6.3 内置 agent 前端 matter（S-03）

见 IF-G4。**新增断言**（S-02 或 S-03 内的测试，owner 按文件归属决定：断言写在 `subagent/mod_test.rs` 则归 S-02，因此 S-03 先落 `.md`、S-02 后加断言）：
- `web_researcher_tools_frontmatter_uses_effective_names`：读 `built-in/web-researcher.md`，断言 `tools` 列表含 `mcp__web__WebFetch` 与 `mcp__web__WebSearch`，且**不含**裸名 `WebFetch`/`WebSearch`。
- `web_researcher_capability_is_writes`：`infer_agent_capability` 对该 frontmatter 的 `can_mutate == true`（因为名单里仍有 `Bash`/`Write`）。这条同时是 IF-G2 第 2 条的落地断言——若将来有人把 `Bash`/`Write` 从该 agent 移除，标签会翻转为 `readonly`，测试会提醒复核 `11_subagent` 的 catalog 文案。

### 6.4 内置 skill 文本（S-05）

`use-artifacts/SKILL.md` 现教：
- `:41` 「`artifact` is a deferred tool. The first call requires two steps」
- `:46-47` `SearchExtraTools({ query: "select:artifact" })` → `ExecuteExtraTool({ tool_name: "artifact", params: {...} })`
- `:54` 第二次 `ExecuteExtraTool({ tool_name: "artifact", params: {..., hash } })`

迁移后 `artifact` 经 IF-D9 被提升为 **direct**：不再需要 `SearchExtraTools` 预取 schema，且 `ExecuteExtraTool` 的 `tool_name` 必须是 `mcp__artifact__artifact`。

**不改会怎样（具体）**：模型照 skill 走两步流程 → ① `SearchExtraTools({query:"select:artifact"})` 找不到该工具（direct 工具不进 deferred 索引）→ 返回空/无关结果；② 随后 `ExecuteExtraTool({tool_name:"artifact"})` 在 deferred 索引中查不到 → 调用失败。即**artifact 功能在 skill 路径上完全不可用**，而这是该能力在提示词层的唯一使用指南。

S-05 的改动范围冻结为：`:4`（description 里的「deferred tool」表述）、`:41`、`:43-47`、`:50-54` 的调用形态；**不动** `:13-33`（何时使用 artifact）、`:113-115`（安全纪律与 URL 形态）。并要求改后至少一条用例可核对（见 §7 的 S-05 验证命令）。

**待核实（不得据此改文本）**：`:114` 声称存在 `/artifacts` slash 命令以列出本会话上传的 artifact。已核实：`peri-tui/src` 与 `peri-acp/src` 的 `*.rs` 中不存在 `"artifacts"` 命令注册。核实方法 = 搜索 `CommandRegistry` 的注册点（`peri-acp-types/src/command_registry.rs` 及其调用者）。结论落地**前**不对该行做任何改动，也不把它登记为关闭面（与 F §6.3 一致）。

### 6.5 ACP `ToolKind` 投影（S-07）

`tool_projection.rs:62-68` 把 `WebFetch`/`WebSearch` 映射为 `ToolKind::Fetch`。迁移后裸名不再出现 → 落到 `_ => ToolKind::Other`。

**不改会怎样（具体）**：ACP 客户端（含 Peri 自己的 TUI 与任何外部 ACP IDE 客户端）收到的工具卡片类型由 `Fetch` 变 `Other` → 客户端的图标/分组/折叠策略失效，且这是**跨客户端**的可见行为变化，比 TUI 内部渲染更难发现。

S-07 的落地（**A4 归一，不再「二选一硬编码」**）：在 `infer_tool_kind`（`tool_projection.rs:61-70`）**入口**先做一次匹配型归一（`original_tool_name_of_effective(name).unwrap_or(name)`），既有 `match` 的 `"WebFetch" | "WebSearch" => ToolKind::Fetch` **保持字面量不变**（改动最小、无新的字面量分支）。**必须**附反证用例「未知 / 外部 `mcp__*`（如 `mcp__foo__bar`、`mcp__web__fetch`）仍为 `Other`」，避免把其它 server 的工具误吞。**不得**引入 `match` 上的 `starts_with("mcp__web__")` 前缀通配，也**不得**在分支里写 `mcp__web__*` 字面量（A4 禁止按名硬编码）。

### 6.6 TUI 按名分支（S-08，**A8：本波在范围内**）

四处消费（§2.2 已逐行核实）：`tool_display.rs:53`（`WebSearch`）、`:58`（`WebFetch`）；`truncate.rs:186`（`WebSearch` 的 `summarize_input` 字段提取）、`:190`（`WebFetch`）、`:275`（`WebFetch` 输出折叠）。

**不改会怎样（具体，逐处）**：
- `format_tool_args` 未命中 → 返回空字符串 → 工具卡片**没有参数摘要**，用户只看到 `mcp__web__WebFetch`（`format_tool_name` 对未知名原样返回，`tool_display.rs:8-14`）而看不到 URL/query。对 WebSearch/WebFetch 这类「参数本身就是用户最想看的上下文」的工具，信息损失最明显。
- `summarize_input` 未命中 → 落到 `_` 分支（`truncate.rs:235-`）的通用 JSON 摘要 → 摘要变长且含无关字段（对比 `WebSearch` 只留 `query` 的 60 字符截断）。
- 输出折叠 `WebFetch` 未命中 → 输出走通用截断策略（默认 200 字符，`:301`），与 `:275-285` 的专门折叠规则（保留 URL 行）不一致 → 同一页面内容在迁移前后呈现不同。
- `SearchExtraTools` 的摘要同样退回通用 JSON（同一归一入口的另一处受益者）。

**S-08 的落地（A8，冻结）**：在**上述每一处按名分支的入口**做一次**匹配型归一**——`let name = original_tool_name_of_effective(raw).unwrap_or(raw);`（`peri-tui` 直接依赖 `peri-acp-types`；该 helper 是 `pub` 纯查表，无新依赖）——然后走**原有**分支。**禁止**把 pattern 改成 `mcp__web__WebFetch` 之类的字面量（A4/A8：会随声明表漂移而失效，且无法覆盖 scale 到后续实例）。同步改 `tool_display_test.rs` / `truncate_test.rs` 的 fixture 与断言，并**新增**三条：
1. `tui_web_tools_still_summarize_after_migration`：effective name 传入时仍产出参数摘要 / 专用折叠（非空摘要、URL 保留）。
2. `tui_unknown_mcp_names_fall_back_to_generic`（反证）：`mcp__foo__bar` 走通用路径，行为与今天一致。
3. `tui_has_no_hardcoded_effective_name`：断言实现里没有 `mcp__web__` / `mcp__artifact__` 字面量（可 grep 的事实）。

`kit/tool_semantics.rs` 与 `kit/acp_types/current_turn.rs` 经核查**无** Web/artifact 按名分支 ⇒ 本波**不改**（不凭空增加改动项）；若实施时发现新的按名分支，一律按同一归一规则处理并回本表登记。

**范围**：只改这 4 个文件。`tool_semantics.rs` / `acp_types/tool_card.rs` / `acp_types/current_turn.rs` / `message_area/render/tool_card.rs` **不改**（§2.2 已核实不含迁移对象名，且它们对未知名落的 `Generic` 路径在迁移前后完全一致）。

**不覆盖声明**：本 task **不**让 TUI 对 `mcp__*` 工具产生「更好的」通用展示（例如自动剥前缀显示 `WebFetch`）。那是产品改进，不是迁移等价，另立 issue。

### 6.7 `--disallowed-tools` / agent `tools:` / hooks（用户面）

- `ToolFilterPolicy::canonical`（`tool_catalog.rs:118-149`）：小写化后**精确**比较 → **A4 ⑦ 匹配型归一**（原样优先、未命中再用原始名），用户写 `--disallowed-tools WebFetch` 或 agent `tools: [WebSearch]` **继续生效**，写 `mcp__web__WebSearch` 同样生效。owner **S-02**。
- hooks matcher（`hooks/matcher.rs:10-26`、`:32-52`）：大小写敏感、精确或正则 → **A4 ⑥ 匹配型归一**（matcher 模式侧与 tool_name 侧都「原样优先」），用户 hook 写 `"WebFetch"` 与写 `"mcp__web__WebFetch"` **都生效**；未知 `mcp__*` 的行为分毫不变。owner **S-01**。
- **本波不做兼容别名**（理由见 F §11 非目标）；归一**不是**别名：它不产生第二个可调用名字，只影响判定与匹配。用户面文档（`docs/reference/mcp-ecosystem.md`，owner **V-05**）说明两种写法都可用。
- `peri-tui/src/cli_integration_test.rs:325-328`：**不改**。它只断言 CLI 参数解析成 `Vec<String>`，`"WebFetch"` 在这里是任意字符串 fixture，与工具是否存在无关；改它属于无意义 churn。

### 6.8 声明段（prompt declaration）投影 —— **A9：不得静默丢失**

归 **主 plan S-02**，G 不持有实现。**A9 后的结论（旧「已知缺口」表述作废）**：迁移后 `collect_declarations`（`tool_search/declaration.rs:16-40`）对三个 builtin 工具**仍必须产出**声明——机制 = 声明文本进声明表（`BuiltinMcpTool::prompt_declaration`，纯数据）+ builtin 工具的 direct 桥实现 `prompt_declaration()`（`McpToolBridge` 对 builtin 来源返回 `builtin_prompt_declaration()`）。`{{name}}` 按既有渲染规则（`:47-75`）替换为 **effective name**。G 只登记验收行（`cargo test -p peri-middlewares --lib -- tool_search::declaration`）与该原则；断言细节见 F §6.5。这一条与 S-05（skill 文本）**互补而非重复**：skill 是「何时/如何使用 artifact」，声明段是「工具一句话定位」。

### 6.9 与 S-01 并行的边界

`permission/mod.rs` 是单文件单 owner，因此**不得**为了并行把 `sensitive_tool_entries()` 拆给另一个 task。若主 plan 需要更细的提交切分，只允许在同一 owner 内做多次提交，不新增重叠任务。

### 6.10 能力关闭（IF-D10）：G 只提供判定语义与验收行，不提供落点

> 本条按主 plan §12「G：… 能力关闭（IF-D10）的四个面」登记，但**实现归属以主 plan §4 为准**：`closed_instances` / `is_closed` 的实现函数在 `peri-middlewares/src/mcp/builtin/mod.rs`（**E-02** 拥有），四个面的**过滤落点**分属 **E-03**（启动提交的 required 集合、`mcp/middleware.rs` 的 deferred bridge 收集）与 **I-03**（`assembly/preparation.rs` 的 parent_tools、**workflow agent 工具面**）。

G 的职责（三条，不得越界）：
1. **判定语义的唯一来源**：`closed_instances(disabled_middlewares)` 必须遍历 `BUILTIN_MCP_INSTANCES` 并按 `policy_key` 匹配（IF-D4 + IF-D10 第 1 条；`policy_key` 的合法集合 = `BUILTIN_INSTANCE_POLICY_KEYS`，A7）。**禁止**任何按字符串硬编码 `mcp__web__` 前缀的过滤（R6）。G 负责在验收里断言这一点（把「只按 `policy_key` 映射」变成可 grep 的事实）。
2. **四个面必须同时过滤**（IF-D10 第 2 条）：① 启动提交的 required 集合；② `mcp/middleware.rs` 的 `static_tool_bridges`（deferred 收集）；③ `assembly/preparation.rs` 的 `parent_tools`；④ **workflow agent 工具面**——**A6 后 workflow 面必须显式过滤**（本波要把 builtin bridge 接进 workflow agent，否则其 Web 能力净丢失）；「workflow 天然关闭」的旧说法**作废**。
3. **关闭集不得影响 readiness**（IF-D10 第 3 条）：`await_system_connections` 仍按 `pool.configs`（pool 级）判定实例 ready，避免 `replace_static_mcp_tools` 的 `RequiredToolUnavailable` 把有意的关闭误报成启动失败。

**关闭/启用的三条路径（A2/A17/A18，冻结）**：
1. `BUILTIN_INSTANCE_POLICY_KEYS` 的对应策略键置 `false`（`"WebMiddleware"` / `"ArtifactMiddleware"`）→ 该实例进关闭集；
2. 用户配置 `{"web": {"disabled": true}}`（**唯一合法关闭片段形状**；`{"web": {"disabled": true, "system_mcp": true}}` 必须被加载期拒绝，A18）→ 实例注册为 `Disabled`；
3. `PERI_MCP_BUILTIN=off`（**A2**）→ 两实例都不注入 ⇒ 能力面为零，**不是**回退到 middleware 实现（提供面已删）。

**必须交付的 presence/absence 矩阵**（IF-D10 第 5 条，四组输入 × 四个面）：`WebMiddleware=false` / `ArtifactMiddleware=false` / 两者都 false / `McpMiddleware=false`。承担测试：crate 内 `mcp::builtin_apply`（配置与关闭语义）+ `mcp::builtin_runtime`（运行时四个面）+ host seam `host::mcp_v4_builtin`（首个 LLM 请求入参）。G 只登记这些验收行，不新建测试文件。

**面板语义分层（必须写进 acceptance）**：关闭 Web 实例后 TUI 的 MCP 面板**仍**显示该实例 connected（它是 pool 级事实）。这是**有意**的语义分层，不是关闭不完整；但必须与「工具面已归零」并列写出，避免被读成矛盾。

## 7. 任务表

依赖用 `→` 表示。**所有命令在仓库根目录运行，仅供后续实施，本轮未执行。** 未列出的文件不得修改（plan §9 规则 1）。

| 批次 | Task | 标题 | owner 产出文件 | 依赖 | 验证命令（精确过滤器） |
| --- | --- | --- | --- | --- | --- |
| W1 | **S-05** | 内置 skill 文本（deferred → direct） | `peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md` | — | 人工核对 + `git diff --check`（无代码断言，见 §7.1） |
| W1 | **S-06** | prompt 段落点名位置同步 | `peri-acp/prompts/sections/01_intro.md`、`11_subagent.md` | — | `cargo test -p peri-acp --lib -- host::prompt::tests`；`cargo test -p peri-acp --lib -- prompt::tests` |
| W2 | **S-01** → 主 plan **S-01** | 策略一致性核心（判定型 ①②③ + 匹配型 ⑥ = `hooks/matcher.rs`；条目表 `name` 与 description；反证/等价性/保留名反例） | `peri-middlewares/src/permission/mod.rs`、`permission/mod_test.rs`、`subagent/mod.rs`、`subagent/mod_test.rs`、`hooks/matcher.rs` | 主 plan **E-01（helper）/ E-02** → | `cargo test -p peri-middlewares --lib -- permission::tests`；`cargo test -p peri-middlewares --lib -- subagent::tests`；`cargo test -p peri-middlewares --lib -- hooks::matcher`（**精确**；裸 `permission`/`subagent` 命中过宽，主 plan §9 规则 4） |
| W3（主 plan W3） | **S-07** → 主 plan **S-02** | ACP `ToolKind` 投影（A4 归一） | `peri-acp/src/event/tool_projection.rs`、`event/mapper_test.rs` | 主 plan E-01 → | `cargo test -p peri-acp --lib -- event::mapper` |
| **W3**（**A8 已裁决，不再是「待裁决」**） | **S-08** | TUI 按名分支（A8：**本波在范围内**，走 A4 归一，不得硬编码 `mcp__web__*`） | `peri-tui/src/kit/tool_display.rs`、`kit/tool_display_test.rs`、`truncate.rs`、`truncate_test.rs` | S-01（helper 由 E-01 提供）→ | `cargo test -p peri-tui --lib -- kit::tool_display`；`cargo test -p peri-tui --lib -- truncate` |
| W3 | **S-03** | `web-researcher` 名单与正文 | `peri-middlewares/src/subagent/built-in/web-researcher.md` | S-01 → | 人工核对 + 由 S-02 的断言承担（见下） |
| W3（主 plan W3） | **S-09** → 主 plan **S-02** | `TOOL_PARAM_ALIASES` 归一（A4 ⑤） | `peri-agent/src/tools/invocation.rs`、`invocation_test.rs` | 主 plan I-03 → | `cargo test -p peri-agent --lib -- tools::invocation` |
| W3（主 plan W3） | **S-02** → 主 plan **S-01 / S-02** | subagent mutation 一致性（③）+ `MUTATION_CORE` 守护 + agent 断言 + ⑦ `ToolFilterPolicy::canonical` + 两表（`MIDDLEWARE_NAMES` 删两键 + `BUILTIN_INSTANCE_POLICY_KEYS` + `provider/config.rs` 键集合） | `peri-middlewares/src/subagent/mod.rs`、`subagent/mod_test.rs`、`peri-agent/src/session/tool_catalog.rs`、`peri-acp-types/src/meta_harness.rs`、`peri-acp/src/provider/config.rs`、`config_test.rs` | S-01、S-03 → | `cargo test -p peri-middlewares --lib -- subagent::tests`；`cargo test -p peri-agent --lib -- session::tool_catalog`；`cargo test -p peri-acp-types --lib -- meta_harness`；`cargo test -p peri-acp --lib -- provider::config` |
| W3（主 plan W3） | **S-04** | 段落渲染锁定复核（`10_hitl` 含两个 web effective name、不含裸名；A19） | `peri-acp/src/prompt/prompt_test.rs` | S-01 → | `cargo test -p peri-acp --lib -- prompt::tests` |
| W5（主 plan W5） | **S-10** → **主 plan V-05** | 用户面文档（名称 / 关闭三条路径 / 保留名 / hook 与过滤的两种写法 / `PERI_MCP_BUILTIN`） | `docs/reference/mcp-ecosystem.md`（**仅此一处**；`docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 同在 V-05 内） | S-01、S-05、S-07、S-08、S-10 → | `git diff --check` + 链接检查 + 人工核对清单 |

> **owner 与波次的最终来源是主 plan §4/§6/§7**（A19）；本表与之对齐：S-03（W3）、S-04（W3）、S-05（W1）、S-06（W1）、S-08（W3）、S-10（V-05，W5）。

### 7.1 每个 task 的断言内容

**S-01**（`mcp::permission` 之外的模块名为 `permission`；既有测试模块是 `permission::mod_test` 的挂载，过滤器用 `permission`）：

1. **等价性测试**（新增，名字冻结）：`builtin_effective_names_match_original_name_policy`
   对 `BUILTIN_MCP_INSTANCES` 的每个 `(instance, tool)`：
   `default_requires_approval(eff) == default_requires_approval(raw)`、
   `is_edit_tool(eff) == is_edit_tool(raw)`。
   （`is_mutation_tool` 的等价性在 S-02 内断言，因为该函数属于 `subagent`。）
2. **冻结结果表测试**：`wave1_frozen_effective_name_policy`
   逐项断言 `default_requires_approval("mcp__web__WebSearch") == true`、`("mcp__web__WebFetch") == true`、`("mcp__artifact__artifact") == false`；`is_edit_tool` 三者均 `false`。
3. **反证测试（未知 `mcp__*` 保守语义不变）**：`unknown_mcp_prefix_remains_conservative`
   断言 `mcp__filesystem__read_file`、`mcp__github__create_issue`、`mcp__unknown__anything`、`mcp__a__b`、`mcp__x__y__z` 全为 `true`；`mcp_`、`mcp`、`mcp_read_resource` 全为 `false`。**并且**断言 `original_tool_name_of_effective("mcp__web__fetch") == None`（小写 `fetch` 不命中表）而 `default_requires_approval("mcp__web__fetch") == true`——即两条不同理由的 true 必须各自成立。
4. **既有断言不得改写**：`permission/mod_test.rs:92-134`、`:436-482` 全部原样通过；条目计数仍为 14、前缀条目仍为 3（仅**探测名**按 A19 更新为 effective name，见 IF-G3）。
5. **保留名反例（A3，新增）**：`builtin_reserved_name_is_not_parity_hijackable`——构造「外部 server 名 `artifact` + 工具 `artifact`」「外部 server 名 `web` + 工具 `WebSearch`」两种输入，断言要么加载期失败（保留名 typed error；配置侧断言落 `mcp::builtin_apply`），要么 `default_requires_approval("mcp__artifact__artifact") == true`（不被当作 builtin 一等工具放行）。
6. **匹配型 ⑥ 的断言（新增）**：`hooks_matcher_accepts_both_naked_and_effective_names`——`matches_matcher("WebFetch", "mcp__web__WebFetch") == true`、`matches_matcher("mcp__web__WebFetch", "mcp__web__WebFetch") == true`、`matches_if_condition("WebFetch(...)", "mcp__web__WebFetch") == true`，且未知 `mcp__foo__bar` 的匹配行为与今天逐位一致（反证）。
7. **禁止硬编码（可 grep 的事实）**：实现中不得出现 `mcp__web__` / `mcp__artifact__` 字面量（检查方式：`grep -rn "mcp__web__\|mcp__artifact__" peri-middlewares/src/permission peri-middlewares/src/subagent peri-middlewares/src/hooks` 必须 0 命中）。
8. **可选分支 O1**（仅主 plan 裁决启用时）：条目表 14 → 15 + 计数断言同步 + §3 IF-G2 第 1 条的记录义务。**缺省不启用**（R17）。

**S-02**：

1. `mutation_tool_matches_original_name_policy_for_builtin_names`：对声明表逐项 `is_mutation_tool(eff) == is_mutation_tool(raw)`；并断言 `is_mutation_tool("mcp__web__WebSearch") == false`、`is_mutation_tool("mcp__web__WebFetch") == false`。
2. `unknown_mcp_prefix_still_mutates`（反证）：`is_mutation_tool("mcp__filesystem__write_file") == true`、`is_mutation_tool("mcp__web__fetch") == true`、`is_mutation_tool("mcp__anything") == true`。
3. `capability_whitelist_mcp_prefix_is_still_writes`：**保持** `subagent/mod_test.rs:495-500` 的既有断言不变（用 `mcp__files` 这类不在声明表内的名字）。
4. `mutation_core_disjoint_from_builtin_tools`：`MUTATION_CORE` 与全部 `BUILTIN_MCP_INSTANCES` 的工具名（小写化后）**交集为空**。
5. `web_researcher_tools_frontmatter_uses_effective_names` 与 `web_researcher_capability_is_writes`（§6.3）。

**S-04**：

1. 既有 `test_hitl_section_rendered_by_holder`（`prompt_test.rs:225-247`）**不改断言**、必须通过。
2. 改写 `hitl_sensitive_list_has_no_migrated_tool_names` → **`hitl_sensitive_list_uses_effective_names`**（A19）：断言渲染出的敏感列表**含** `mcp__web__WebSearch` / `mcp__web__WebFetch`（条目的 `name` 已改为 effective name），**不含**裸名 `WebFetch` / `WebSearch` / `artifact`，**不含** `mcp__artifact__artifact`（artifact 不在敏感清单），且条目总数仍为 14 / 前缀 3。
3. 若启用 O1：新增断言列表含 `artifact`（**缺省不启用**）。

**S-05 / S-06 / S-03 / S-10**：这些是文本改动，**没有**自动断言可承担全部正确性。处置：
- S-05 / S-03 / S-10 的验证 = `git diff --check` + 链接检查 + 人工核对清单（逐条列出改了哪一行、改了成什么、为什么）。**不得**声称「测试通过」。
- S-06 的验证命令用既有的 `host::prompt::tests` 与 `prompt::tests`，其作用是**回归**（证明段落渲染未被破坏），不是「证明文本正确」。
- owner 必须在完成报告里写出「本次改动无自动断言覆盖，证据为人工核对」——这是本波唯一允许的弱证据形态，且必须显式声明。

**S-07 / S-08 / S-09**：断言见 §6.5（归一，不得硬编码）/ §6.6（A8：三处 TUI 断言）/ IF-G5（匹配型归一 + 无字面量 grep）。

## 8. 执行批次与依赖

| Wave | 并发 task | 同 crate 冲突 | 说明 |
| --- | --- | --- | --- |
| **W1** | S-05、S-06 | 无（不同 crate） | 纯文本，可与 F 的 W1 并行（主 plan §7 W1 同列） |
| **W2（主 plan）** | S-01 | `peri-middlewares`（S-01：`permission/`、`subagent/`、`hooks/matcher.rs`） | 主 plan §7 W2 = E-03 + I-01 + S-01；S-07/S-09 属 S-02（W3）。三者都读注册表与 IF-D15 helper（E-01） |
| **W3（主 plan）** | S-03、S-04、S-07、S-08、S-09 | `peri-middlewares`（S-03）、`peri-acp`（S-04、S-07）、`peri-tui`（S-08）、`peri-agent`（S-09） | S-08（TUI）**已按 A8 纳入**；各 task 文件互斥 |
| **W3（主 plan）** | S-02（含 ⑦ `canonical` 与两表改造） | `peri-middlewares`（S-02 的 subagent 部分由主 plan S-01 持有）、`peri-agent`、`peri-acp-types`、`peri-acp` | S-02 依赖 S-01 与 S-03 都已落地 |
| **W5（主 plan）** | S-10 → **V-05** | `docs/` | 汇总用户可见结论，必须最后做（与 code-index/stdards 同步同属 V-05） |

**G 与 F 的先后与可并行性（明确回答）**：
- **硬依赖**：G 的归一读 `BUILTIN_MCP_INSTANCES` 与 IF-D15 helper（主 plan **E-01**）以及 `mcp::builtin::effective_tool_name`（**E-02**）。两者不落地，G 无法编译。
- **硬依赖（已消除）**：~~S-01 需要先释放 `peri-middlewares/src/lib.rs`~~ —— G 不写该文件（头部映射表）。
- **可并行**：主 plan §7 W2 的 E-03 / I-01 / S-01 文件互斥；G 的 S-07/S-09 属 S-02（W3，依赖 I-03）。
- **G 不等待 builtin runtime**：策略与投影改动只依赖注册表与 effective name 规则，不依赖 E-03 的 spawn / duplex 运行时。
- **无 `lib.rs` 交叠（已消除）**：G 不写 `peri-middlewares/src/lib.rs`（该文件唯一 owner 是主 plan I-03）。
- **建议的收敛顺序**：主 plan E-01 → E-02 → （W2）E-03 / I-01 / S-01 → （W3）I-02 → I-03 → S-02 → （W4）V-01 / V-02。G 的 S-01 落在 W2；S-03/S-04/S-07/S-08/S-09/S-02 落在 W3；S-05/S-06 在 W1 先行（纯文本）；S-10 随 V-05 收尾。

## 9. 验收矩阵（本 sub-plan 的诚实分级）

| 交付判据 | 主责任务 | 断言层次 | 证据强度 |
| --- | --- | --- | --- |
| 三个 effective name 的判定等于原始名判定 | S-01、S-02 | 纯函数单测（等价性 + 冻结结果表） | **强**：判定函数是纯函数，单测即全覆盖 |
| 未知 `mcp__*` 保守语义不变 | S-01、S-02 | 纯函数单测（反证用例） | **强**（前提：既有 `mod_test.rs:92-134` / `:495-500` 不被改写，S-01/S-02 的报告必须贴出「未改这些行」的证据） |
| `sensitive_tool_entries()` 与段落同步 | S-01、S-04 | 条目表 ↔ 判定函数一致性测试 + 段落逐字渲染 | **强**（两个既有锁定测试构成双锁） |
| `ToolKind` 不退化为 `Other` | S-07 | 映射函数单测 + 反证 | **强** |
| TUI 参数摘要/折叠不退化为通用路径（**A8**） | S-08 | 归一 + 格式化函数单测 + 反证（未知 `mcp__*` 仍走通用路径）+ 「无硬编码字面量」grep | **中强**：证明函数分支正确；**不**证明 TUI 端到端渲染正确（未跑 `peri-tui` 集成/e2e） |
| 参数别名在迁移后仍生效（A4 ⑤） | S-09 → S-02 | 归一化函数单测 + 声明表锁定 + 反证 | **中强**：`normalize_params` 是纯函数，覆盖强；真实模型是否输出 `search_term` 无证据 |
| 内置 agent 名单正确 | S-03 + S-02 | frontmatter 解析 + `infer_agent_capability` | **强**（名单裁剪与能力标签都是可断言的） |
| 内置 skill 文本正确 | S-05 | **无自动断言** | **弱**：人工核对 + `git diff --check`。owner 必须显式声明 |
| prompt 段落点名位置正确 | S-06 | 回归命令 | **弱**：命令证明渲染未坏，不证明文本正确 |
| 用户面（hooks / `--disallowed-tools` / agent 名单 / 保留名 / `PERI_MCP_BUILTIN`）影响已声明 | S-10 → V-05 | 文档 + 链接检查 | **弱**：文档存在性，不证明用户已迁移 |
| 保留名不可被外部 server 接管（A3） | S-01 + I-02 | 反例用例（配置侧 + 判定侧） | **强**：两种结果都可断言 |
| 两表形态（A7） | S-02 | 集合断言（槽位名 / 策略键 / 交集）+ `provider/config.rs` 键合法性 | **强** |
| 声明段仍产出（A9，G 侧只登记） | S-02（主 plan） | `tool_search::declaration` | **中强**（实现与断言在 F §6.5 / 主 plan S-02） |

**本 sub-plan 明确不覆盖**：
- 不覆盖「关闭能力后全部关闭面消失」——归 sub-plan H 的运行时用例（F §6.3 的面 1/2/6/7）。
- 不覆盖 builtin 实例的凭据隔离（`McpClientHandle` 无 credential 字段，v4-part-1 acceptance §3 已记录为 UNVERIFIED）。
- 不覆盖 `peri-tui` 的端到端渲染与 e2e 场景（本波不跑）。
- 不覆盖用户既有 hook 配置的实际迁移（无法自动探测用户配置）。

## 10. 风险与未知

| # | 风险 | 影响 | 缓解 |
| --- | --- | --- | --- |
| G-R1 | 实现 IF-G1 时把 `mcp__` 前缀分支改成「枚举 builtin 例外」 | 未知 `mcp__*` 的保守语义被改坏，且 `sensitive_entries_match_default_requires_approval` 会红 | S-01 的验证命令**必须**包含 `permission::tests` 与 `subagent::tests` 精确过滤器；报告需断言条目计数仍为 14/3 |
| G-R2 | `mcp__artifact__artifact` 变为不需审批被读成「本波放宽了安全边界」 | 误判为安全回归 | IF-G2 第 1 条要求显式记录；acceptance 必须写出「迁移前 `artifact` 亦不需审批」的依据与「若需收紧走 O1」的路径 |
| G-R3 | 归一被实现为硬编码 effective name 字符串或第二张反查表（**A4 冲突**） | 与声明表漂移后静默失效 | IF-G1 第 1 条 + IF-G5 的锁定测试（遍历声明表 + 反证 + 「无 `mcp__web__` 字面量」grep）；**任何消费点都不例外** |
| G-R4 | TUI 分支把 pattern 改成 `mcp__web__*` 字面量（A8 冲突） | 声明表漂移即失效，且无法覆盖后续实例 | §6.6 冻结为「入口归一 + 保留原分支」；新增 `tui_has_no_hardcoded_effective_name` 断言 |
| G-R5 | 文本类改动（S-03/S-05/S-06/S-10）被报成「测试通过」 | 假绿 | §7.1 强制弱证据声明；完成报告必须写明「无自动断言」 |
| G-R6 | 保留名被外部 server 接管后继承 parity / 非法关闭片段（A3/A18） | 审批门被静默移除；或所有 session 被 `Err(Disabled)` 阻断 | 加载期 typed error + 反例用例（I-02 的 `mcp::builtin_apply` + S-01 的判定侧反例） |
| G-R7 | `web-researcher.md` 只改了正文没改 frontmatter（或反之） | 前者低危，后者使该 agent 失去全部 Web 工具 | IF-G4 把两者拆成「功能级/文案级」并各给后果；S-02 的 frontmatter 断言钉住功能级 |
| G-R8 | 范围蔓延到「全仓改名」 | 波及 `Bash`/`Read` 等未迁移名字，制造大量无意义 diff 与回归 | §6 逐面核实 + §2.2 的「否」行 + §11 非目标；owner 报告需列出「本次未改动的名字」 |
| G-R9 | `/artifacts` slash 命令的存在性未定 | 关闭面清单可能漏一项 | 已登记为待核实（§6.4）；结论落地前不改文本、不登记为关闭面 |

## 11. 非目标

- **不**改任何未迁移工具的名字（`Bash` / `Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations` / `Agent` / `TodoWrite` / `SkillTool` / `DiscoverSkillsTool` / `LSP` / `goal` / `Workflow` / `AskUserQuestion` / `DiscoverMCP` / `mcp_read_resource` / `SearchExtraTools` / `ExecuteExtraTool` / `RunPtcCode` / `DynamicMCP.*`）。
- **不**为迁移工具引入裸名别名（`McpToolBridge::aliases()` 保持返回空；F §11 非目标同款理由）。**注意区分**：A4 归一只影响判定与匹配（审批 / 过滤 / hook / TUI 展示 / 参数别名），**不**产生第二个可调用名字——因此「匹配型归一」不违反本条。
- **不**做用户 hook 配置、`--disallowed-tools`、agent `tools:` 名单的自动迁移（无法可靠探测用户配置；只做文档声明）。**注意**：归一后**两种写法都继续生效**（A4 ⑥⑦），因此文档要写清「无需迁移，但推荐改用 effective name」。
- **不**在 `peri-tui` 内硬编码 `mcp__web__*`（A8）；TUI 改动面限定 `kit/tool_display.rs`、`truncate.rs` 与其测试。
- **不**改 `tool_semantics.rs` / `acp_types/tool_card.rs` / `acp_types/current_turn.rs` / `message_area/render/tool_card.rs` / `cli_integration_test.rs`（§2.2 已核实不含迁移对象名）。
- **不**改 `sensitive_tool_entries()` 的**条目数量与顺序**（A19 只改两个已迁移条目的 `name` 与 `mcp__` 条目的 description；除非启用 O1）。
- **不**改 `05_using_tools.md` / `07_runtime.md` / `10_hitl.md`（已核实只点名 `Bash` / `Write` / `Edit` / `folder_operations`）。
- **不**改 `ultracode/SKILL.md`（其中 `artifact` 为泛指名词）。
- **不**改 `explorer.md` / `plan.md` / `coder.md` / `general-purpose.md` / `verification.md` 的 frontmatter。
- **不**改 `core_mutation_tools_fully_disallowed` 的集合（只加守护测试）。
- **不**改 `docs/design/**`（设计文档不回填）；`docs/reference/mcp-ecosystem.md`、`docs/code-index/**`、`docs/standards/**`、`CLAUDE.md` 由 **V-05**（主 plan）同步（A19：已登记 owner 与 doc 检查项）。
- **不**改 `example/minimal/**`（F-IF-F6 的活体样本在被删除前仅作阅读参考）。
- **不**默认启用可选分支 O1（artifact 收紧为需审批）——需主 plan 显式裁决（R17：缺省不启用）。
- **不**改 `sensitive_tool_entries()` 的条目数量与顺序（A19 只改两个已迁移条目的 `name` 与 `mcp__` 条目的 description；除非启用 O1）。

## 12. 对 IF-D4 / IF-D6 / IF-D7 / IF-D9 与新增冻结接口的落地结论

| 冻结接口 | 结论 | 落地位置 / 偏差 |
| --- | --- | --- |
| **IF-D4** builtin 注册表（数据 / 行为分离） | **成立，且 G 是其唯一策略消费者**。G 只读 `BUILTIN_MCP_INSTANCES`（`peri-acp-types`）与 `mcp::builtin::effective_tool_name`（`peri-middlewares`，`pub(crate)`），**不**在 G 侧维护第二份名单 | IF-G1（归一 helper，**A4**）、IF-G5、S-07/S-08 的分支都从注册表派生。⚠ 本文早期版本主张把规则上移到 `peri-acp-types` 并让 `tool_bridge.rs` 委托——**不采用**（主 plan IF-D4 冻结反向依赖）；`peri-acp-types` 只持有冻结字面量与**查表** helper（IF-D15） |
| **IF-D6** 判定相等 + 未知 `mcp__*` 不变 | **成立；形状按 A4 统一为「单 helper + 七类消费点」**（判定型替换、匹配型原样优先）。三条必须显式记录的后果（见 IF-G2）：① `mcp__artifact__artifact` 变为不需审批（与迁移前裸名等价）；② 两个 Web 工具的 `is_mutation_tool` 由 true 变 false；③ 因此依赖「`mcp__` 前缀」的既有推导（如「任何 MCP 工具都算 writes」）**只对未知 MCP 成立** | IF-G1 的 helper（`peri-acp-types`，owner E-01）+ 七类消费点（S-01 ×4 / S-02 ×2 / S-08 ×1）；反证基线为 `permission/mod_test.rs:97-134`、`subagent/mod_test.rs:495-500`；`mcp__` 条目 description 与两个条目 `name` 按 R4/A19 |
| **IF-D7** 静态表与投影同步 | **已由 A7 裁决**：`MIDDLEWARE_NAMES` **删除** `"WebMiddleware"`/`"ArtifactMiddleware"`（只留链槽位名），**新增** `BUILTIN_INSTANCE_POLICY_KEYS` 承载这两个关闭键；`MIDDLEWARE_TOOL_NAMES` 删除三个裸名（本文 IF-F5 的判断成立）；`stage_builder/tools.rs` 谓词重新论证 + `tools_test.rs` 反向断言；`builder_v2_test.rs` fixture 换名；`tool_projection.rs` 走归一；`TOOL_PARAM_ALIASES` 走归一 | **S-02**（`meta_harness.rs` + `provider/config.rs` + `stage_builder/*` + `invocation.rs` + `tool_projection.rs` + `tool_catalog.rs`）。**原「未收口冲突」（F §10 R9）已关闭**：`assembly_test.rs:1207-1217` 的断言改为「槽位名 == `MIDDLEWARE_NAMES`」∧「策略键 == `policy_key` 集合」∧「交集为空」（强度不降） |
| **IF-D9** `system_mcp` + `system_mcp_tools` 保持可见性等价 | **成立**。G 侧只贡献一件事：迁移后工具的**策略判定**与原始名一致，因此「被提升为 direct」不改变审批/能力标签语义（`is_direct` 与审批无耦合——Permission 只看 `(name, input)`） | 无需 G 侧新增接口。**A2**：`PERI_MCP_BUILTIN` 只控制实例注入，G 需断言「该 env **不**改变策略判定面」（对任何名字的 `default_requires_approval` / `is_mutation_tool` 结果不变） |
| **IF-D10** 能力关闭 | **成立**，G 的角色是判定语义 + 验收行，落点归 E-03 / I-03（**四个面**，见 §6.10） | §6.10 |
| **IF-D15** 生效名归一 helper（**本轮新增，A4**） | **成立且是 G 的核心依赖**：`original_tool_name_of_effective`（`peri-acp-types`，owner E-01）是唯一归一入口；G 的七个消费点（§3 IF-G1）全部经它，**禁止**自建第二张表或硬编码 effective name | IF-G1 / IF-G5 / §6.5 / §6.6 / §6.7；断言见 §7.1 |
