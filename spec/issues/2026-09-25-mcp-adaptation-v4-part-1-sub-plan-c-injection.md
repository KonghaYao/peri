# MCP adaptation v4-part-1 — sub-plan C：System MCP 一等工具注入

> **主 plan v2 覆盖（优先于本文）**：见 [`2026-09-25-mcp-adaptation-v4-part-1-plan.md`](2026-09-25-mcp-adaptation-v4-part-1-plan.md) §5。本文与主 plan 冲突处一律以主 plan 为准，涉及本文件的具体覆盖：**R8**（所有新增 seam 统一 `pub(crate)`，本文的 `pub` 表述作废）、**R9**（「通过真实 catalog / `run_reason` 验证首个 Reason」上移到 `peri-acp` host seam 由 B-07 承担；`build_session_tool_view` 是 `pub(super)`，`peri-middlewares` 无法调用）、**R15**（C-INJ-03 已纳入主 plan 任务表）、**R2 相关**（`McpMiddleware` 的闸门由 B-03 接线，本文不改 `middleware.rs`）。

## 1. 元信息

- 日期：2026-09-25；状态：实现规划，未实施、未运行测试。
- 权威目标：`docs/design/mcp-adaptation-v4-part-1.md:33-65,222-234`；规划前已完整阅读该文件 244 行。
- 本 sub-plan 主责验收 **3、4**，并遵守 **7** 的状态表述；整个 workflow 只实施 **1–4、7**。五个 MCP 实例的真实迁移不在此次范围。
- 依赖配置 sub-plan（以下称 A）提供合法、按原始 server identity 分组的 `system_mcp_tools`；依赖 B 提供 1R ready 闸门、发现完成证据、timeout、失败传播及目录发布。A/B 的实际接口尚未在仓库出现，不把建议接口写成现存实现。
- C 拥有 `peri-middlewares/src/mcp/tool_bridge.rs`、新增 `system_tools.rs` / `system_tools_test.rs`，以及 `mcp/mod.rs` 的一行模块声明。**C 不修改 `mcp/middleware.rs`，不修改 Agent、ToolSearch、Dynamic registry 或配置文件。**
- 可验证结果：所属 namespace 内逐项解析必需工具；同一 bridge 被提升为 direct 而非另注册一个副本；首个 Reason 的实际 LLM tools 含必需工具；普通 deferred 仍可搜索；缺失/schema 错误阻止启动；空数组不增加 direct 工具。
- 本次规划唯一写入文件就是本文件；文中 cargo 命令均为未来实施后的验证命令，本次未执行。

设计契约逐条落点：

| 设计契约 | 实施责任与落点 |
| --- | --- |
| initialize、能力协商、tools/list 完成后逐项确认 | B 先证明 discovery 完成；C 的 `prepare_system_tools` 对该快照逐项确认；不能以 Connected 或空 tools 推断成功 |
| 全部 ready 后直接注入，不需搜索 | C 批量验证成功后才修改 bridge direct 标志；B 原子发布完整结果；实际断言在 `run_reason` 的 LLM 入参 |
| 所属 namespace 匹配，继续 effective name | 精确匹配 `(原始 server name, 原始 tool name)`；展示名复用现有命名函数，不注册裸名 alias |
| 缺失/schema/initialize/timeout 均阻止启动 | C 产出工具解析错误；B 包装为 `SystemReadinessError` 并从 1R 返回错误；失败不发布结果、不调用 LLM |
| 空数组只要求 ready | C 对该 server 不做必需工具 schema 检查、不提升任何工具；B 仍做连接、初始化与 discovery 等 ready 检查 |

## 2. 事实基线

### 2.1 可见性与真实分类 seam

1. `BaseTool::is_direct()` 默认为 `false`，`visible_to_model()` 默认为 `true`：`peri-acp-types/src/tools.rs:608-618`。两者是独立条件，direct 不代表可以绕过 model visibility。
2. `McpToolBridge` 当前 **没有 override `is_direct()`**；结构体只有 `model_visible` 等字段（`peri-middlewares/src/mcp/tool_bridge.rs:33-44`），`impl BaseTool` 从 `:179` 开始，`:200-202` 仅 override `visible_to_model()`。当前静态和动态 bridge 均继承 deferred 默认值。
3. ARC-TOOLS-001 指向 session-local 事实源：`docs/standards/architecture-contracts.md:54-58`。实际产出函数是 `peri-agent/src/session/exec/stage_builder/tools.rs:22-46::build_session_tool_view`，可见性为 **`pub(super)`**，不能由 peri-middlewares 的测试直接调用。
4. 该函数本身负责过滤残留 middleware 名称、合并工具，**不负责 direct/deferred 分类**。生产调用在 `peri-agent/src/session/exec/stage_builder.rs:456-458`，先 `chain.collect_tools`，再注册 catalog；`:460-466` 才组装 StageContext。
5. `SessionToolCatalog::finalize` 在 `peri-agent/src/session/tool_catalog.rs:268-276` 以 `is_direct() && visible_to_model()` 生成 `direct_definitions`。真正 Reason 先 refresh、替换 working map、跑目录 hook、pin（`peri-agent/src/agent/stages/reason.rs:22-40`），再以同一条件选择 LLM 入参（`:97-106`）。**不能只断言 bridge 上一个布尔值就宣告验收 3 通过。**
6. ToolSearch 在 `peri-middlewares/src/tool_search/middleware.rs:55-101` 优先读取 local_tools、否则读 shared_tools；只有 `!is_direct()` 进入 deferred_arcs 和 request index；direct 名称传给 Search/Execute 元工具。它只重绑两个元工具，不改写 MCP bridge。`:104-128` 重建共享索引及 prompt contribution，`:150-158` 在 before_agent / before_reason_catalog 均重绑。`ToolSearchIndex::build` 自身不再次分类（`tool_index.rs:189-195`），测试应调用真实 middleware hook，而不是手工先过滤再宣称路径通过。

### 2.2 effective tool name：精确规则

- `McpToolBridge::new` 保存原始 `tool.name` 为 `tool_name`，`full_name = effective_mcp_tool_name(server_name, tool_name)`：`peri-middlewares/src/mcp/tool_bridge.rs:97-119`。
- `effective_mcp_tool_name` 的格式严格为 **`mcp__{S(server)}__{S(tool)}`**：`:89-95`。`S` 逐字符保留 ASCII 字母、数字、`_`、`-`，其它字符每个替换成 `_`，**不折叠大小写、不 trim**：`:49-60`。
- `new_dynamic` 先要求两个原始分量非空且仅含 ASCII 字母数字、`_`、`-`，否则返回 `ToolCallError::Unavailable`，再使用相同 full_name 格式：`:122-154,171-176`。它不是另一种 `DynamicMCP.xxx` bridge 命名格式。
- `BaseTool::name()` 返回 `full_name`；`mcp_server_name()` 返回 **`Some(&原始 server_name)`**，不是 sanitized server，也不是带 `mcp__` 前缀的字符串：`:179-194`。调用 MCP 时使用原始 `tool_name`：`:230-233`。
- `DynamicMcpMiddleware::collect_tools` 只注册控制工具 `DynamicMCP`：`peri-middlewares/src/mcp/dynamic/tool.rs:12,51-62,114-121`。其 bound 操作使用 canonical action 的 policy name（`:273-289`），不是把各 MCP 工具都命名为控制工具的方法。
- 配置解析决策：`mcpServers.workspace.system_mcp_tools = ["Read"]` 只匹配 `client.name == "workspace"` 且 `tool.name == "Read"` 的那一项；暴露 **`mcp__workspace__Read`**。`read` 不匹配 `Read`；不剥离 `mcp__workspace__`，不接受有效名作为原始名的替代写法，不跨 server 搜索，不使用搜索的大小写规则。若 MCP 本身真的声明原始名 `mcp__workspace__Read`，只有同字面配置才匹配，并正常再次加 server 前缀。
- 已有净化回归 fixture：`peri-middlewares/src/mcp/tool_bridge_test.rs:33-60`，如 `plugin.ctx/web.reader → mcp__plugin_ctx__web_reader`。

### 2.3 桥接集合、去重与动态 shadow

- `build_tool_bridges` 遍历所有 connected client 的所有 tools，每项创建一个 bridge，并保留 `handle_generation` 和 `app_binding_leases`：`peri-middlewares/src/mcp/tool_bridge.rs:369-382`；connected 过滤见 `peri-middlewares/src/mcp/client.rs:198-204`。
- `McpMiddleware::collect_tools` 在 `peri-middlewares/src/mcp/middleware.rs:366-385` 先取得上述 bridges，再追加 `McpResourceTool` 和 `DiscoverMCPTool`。静态 bridge 使用 **deployment-owned `tool_pool`**，资源/发现用 session-projected `pool`：`:32-37,64-68`。
- `build_tool_bridges` 没有实际 dedup。其末尾“内置工具优先去重”注释（`tool_bridge.rs:385`）下面没有实现，不能当作证据。
- `build_session_tool_view` 使用 `BTreeMap::insert`，同名是**后写覆盖**：`peri-agent/src/session/exec/stage_builder/tools.rs:40-44`。两份同名 direct/deferred 不会在最终 map 中同时存在，却可能由顺序决定哪份存活；collect Vec 层已经重复注册。不能依赖这层“去重”。
- catalog 的 source 优先来自 `mcp_server_name()`：`peri-agent/src/session/tool_catalog.rs:135-145`；动态 capability 按整个 server 删除静态工具后插入动态工具：`:238-264`；finalize 只额外检查 alias 冲突：`:277-286`。
- 动态 registry 对 effective 名作 ASCII 小写冲突检查：`peri-middlewares/src/mcp/dynamic/registry/capability.rs:29-60,64-99`；跨 catalog 检查刻意排除同 server 静态条目（`:75-77`），因此不能认为 static system MCP 天然不会被 shadow。
- C 的决策：**构建一次 typed bridges，在同一批对象上打 direct 标记，最终只 boxing 一次。** 不额外 append required bridges；不通过全局 registry 修补；不复制动态 bridge 到静态池。

### 2.4 错误与测试脚手架

- 现有 `ToolCallError` 只有 `NotConnected / Unavailable / CallFailed / Timeout`，用于执行期：`peri-middlewares/src/mcp/tool_bridge.rs:11-30,217-247`。不把启动时工具缺失伪装成调用失败，也不增加一个含义模糊的 `Unavailable` 分支。
- 两个现有 constructor 都只是将 `Tool.input_schema` 转成 JSON Value，并 fallback `{}`，没有 schema 结构校验：`tool_bridge.rs:106-107,147-148`。`Tool` 的 input_schema 已是 JSON object 数据；单纯测试 `to_value` 成功无法证明 schema 合法。
- manifest 已有 serde_json、thiserror、rmcp，没有专用 JSON Schema validator：`peri-middlewares/Cargo.toml:14-18,38-43`；workspace manifests 的检索也未找到 jsonschema/schemars 依赖。不得在计划中虚构现有完整 JSON Schema 编译器。
- `tool_bridge_test.rs:4-14` 用 `serde_json::from_value` 构造 `Tool`；`:16-31` 直接构造 `McpClientHandle { peer: None, tools: vec![], ... }`。可无真实 server 构造存在、缺失、语义结构非法 schema 三类 fixture，供 C 纯逻辑测试使用；这些 fixture **不能证明协议 ready**。
- 非法 fixture 应用 `inputSchema: {"type":"object","properties":42}` 等可被 `Tool` 接收、但结构不合法的数据。`inputSchema: []` 可能直接在 rmcp Tool 反序列化失败，属于 B 的 discovery 错误，不能声称 C 接到了这种 Tool。
- 初始化发现存在吞错风险：`peri-middlewares/src/mcp/initialize.rs:218-221,491-494` 对 `list_all_tools_cached` 使用 `unwrap_or_default()`。空数组契约尤其不能依靠“tools 为空且 Connected”绕过失败；C 无法从 bridge 集合恢复被吞掉的错误，必须由 B 解决/携带成功证据。
- `SystemReadinessError` 和 `system_mcp_tools` 在本次源码检索时尚不存在。现有边界支持 `AgentError::MiddlewareError { middleware, reason }`：`peri-acp-types/src/error.rs:40-41`；`Other` 的用户文案是通用错误（`:273-276`），不应让明确的启动诊断只剩通用文案。

## 3. 接口冻结

以下为**拟新增接口**，不是当前已经存在的 API。使用 crate 内可见性，避免为本次需求扩大 public surface。

### 3.1 `tool_bridge.rs`：typed 单次构建与 direct 标记

```rust
impl McpToolBridge {
    pub(crate) fn with_direct(mut self) -> Self;
    pub(crate) fn original_tool_name(&self) -> &str;
}

pub(crate) fn build_typed_tool_bridges(
    pool: &McpClientPool,
) -> Vec<McpToolBridge>;

// 已有 public 接口保持签名及默认 deferred 行为不变。
pub fn build_tool_bridges(pool: &McpClientPool) -> Vec<Box<dyn BaseTool>>;
```

- 新增 private `direct: bool`，`new` / `new_dynamic` 初始化为 false；override `BaseTool::is_direct()` 返回该字段；`with_direct` 只改 direct，不改变 visibility、名字、client、generation、admission 或 leases。
- 为 `McpToolBridge` 增加 `Clone`：仅 clone 原有 String/Value/Arc/gate 等字段，不新建连接。目的是 B 可从已验证快照多次返回新 Box，而不是重复连接或重注册。
- 将原 `build_tool_bridges` 的循环提取到 typed helper，原 public 函数只做 `.map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)`。两种 constructor、generation 和 lease 行为保持原样。
- `original_tool_name` 暴露实际原始名，避免从 effective name 反拆（净化不可逆、`__` 也可能出现在分量中）。server identity 继续用已有 trait 方法。

### 3.2 新模块 `system_tools.rs`

`mcp/mod.rs` 仅新增一行 `pub(crate) mod system_tools;`。

```rust
use std::collections::BTreeMap;
use super::tool_bridge::McpToolBridge;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SystemToolError {
    MissingTool { server: String, tool: String },
    AmbiguousTool { server: String, tool: String },
    InvalidSchema {
        server: String,
        tool: String,
        path: String,
        reason: &'static str,
    },
    NotModelVisible { server: String, tool: String },
    EffectiveNameCollision { effective_name: String },
}

pub(crate) fn prepare_system_tools(
    bridges: Vec<McpToolBridge>,
    required: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<McpToolBridge>, SystemToolError>;
```

**输入/输出不变量：**

1. `required` 只包含经 A 合法化的 System MCP，key 为原始 server name；允许 value 为空。B 必须在调用前确认这些 server 的 transport、initialize、协商及 tools/list 成功。C 不接收 config、不读网络、不等待、不判断 ClientStatus 是否足以 ready。
2. 输入是同一次 `build_typed_tool_bridges(&self.tool_pool)` 的结果。按 raw server/raw tool exact match；重复配置项幂等，不造成重复输出；同一 server 有多个同名原始 Tool 则 `AmbiguousTool`。
3. **先验证所有必需项，再消费输入给选中 bridge 调用 `with_direct()`**。任何错误都只返回 Err，没有部分成功集合；B 不得缓存半成品或先发布 ready。
4. 对被选中的 effective name，检查整批静态 bridges 是否存在第二个同 effective 名或 ASCII case-fold 冲突；冲突返回 `EffectiveNameCollision`。这覆盖 sanitize 碰撞及大小写执行歧义，不改变完全无关普通 deferred 工具的历史冲突策略。
5. 必需项 `visible_to_model() == false` 返回 `NotModelVisible`。不得擅自把 app-only 工具变成模型工具，也不得将“direct=true 但模型不可见”当作成功注入。
6. 返回 Vec 与输入**长度相等、顺序不变、身份不变**，仅 required 对应项 direct=true。空数组不代表删除该 MCP 普通工具：原来的 deferred 工具依旧存在，新增 direct 数量为 0；无需对该 server 的普通 schema 增加校验。
7. 没有额外 `ToolCallError` 变体。C 负责上述解析/结构错误，B 只包装和传播，不再实现第二套工具存在性/schema 检查。

**schema 校验口径：**

- C 在新模块内新增私有结构校验器，针对 `bridge.parameters()` 的 MCP input schema；不改普通 bridge constructor 的历史行为、不增依赖。
- 根必须是 object；`type` 若存在须为 `"object"`，允许 `{}` 这种未声明约束的 object schema。递归 schema 节点必须为 object 或 boolean；校验已知关键字的结构：`type` 为合法 JSON Schema 类型名或非空无重复类型数组，`properties / patternProperties / $defs / definitions / dependentSchemas` 为 schema map，`required` 为无重复字符串数组，`items / additionalProperties / contains / not / if / then / else / propertyNames` 为 schema，`allOf / anyOf / oneOf / prefixItems` 为 schema 数组；字符串引用/标识与数值、布尔约束也按各关键字应有类型检查。`enum` 非空数组，`const` 可为任意 JSON 值。
- annotation、vendor extension、`default`、`examples`、`enum`、`const` 内容不是 schema，不对其中普通数据递归套 schema 规则。错误只包含字段路径和固定原因，不打印整个 schema、默认值或 Tool payload。
- 这是 **MCP 声明的结构解析检查**，不是完整 JSON Schema draft 的元 schema 验证、instance validation 或远程 `$ref` 解析；不得联网解析 `$ref`。目前仓库无统一 draft/validator 策略；若父计划将“schema 无法解析”明确要求为完整 draft 编译，必须先调整接口实现与依赖文件所有权，不能将有限结构检查伪称完整 validator（见第 6 节）。

### 3.3 给 B 的精确接线契约

**责任划分：所有下列 `middleware.rs` 接线均由 B 实施，C 不写该文件。**

1. B 的 1R 闸门在所有 System MCP 的 discovery 成功后，执行：

   ```rust
   let typed = build_typed_tool_bridges(&self.tool_pool);
   let prepared = prepare_system_tools(typed, &required)
       .map_err(|source| SystemReadinessError::RequiredTools { source })?;
   ```

   `required` 来自 A 的有效配置。B 使用 deployment tool_pool，不能换成会混入动态投影的 self.pool。成功结果为**整批静态 MCP bridges**，含标记后的 required 和未改动的 deferred；B 在所有 required server 校验成功后原子保存为本次准入的快照。

2. B 定义并拥有：

   ```rust
   SystemReadinessError::RequiredTools { source: SystemToolError }
   ```

   该 error 类型须与 `SystemToolError` 的 crate 可见性兼容。B 保留 source 链；在既有 AgentResult 边界转换为 `AgentError::MiddlewareError { middleware: "McpMiddleware".into(), reason: safe_error.to_string() }`，保证 server/tool/error 类别明确可见。initialize、discovery、timeout、取消等仍由 B 的其它变体承担，不由 C 假造。不得 `unwrap_or_default`、warn 后继续、panic 或改成成功空集合。

3. **`collect_tools` 修改锚点：当前 `peri-middlewares/src/mcp/middleware.rs:367`。** 无 System MCP 配置时保留 `build_tool_bridges(&self.tool_pool)` 原路径；配置了 System MCP 且已有本次准入快照时，用 `prepared.iter().cloned().map(|bridge| Box::new(bridge) as Box<dyn BaseTool>).collect()` **替换**这一行的初始 Vec。

   **不允许**先调用旧 `build_tool_bridges` 再 extend `prepared`。因为 prepared 已包含所有静态 bridges，再 extend 会重复。随后 `:369-383` 现有 resource/discover push 原样保留，`:385` 返回值类型不变。

4. `collect_tools` 返回 Vec，**不能传播 Result**；所有工具解析错误必须在 B 的 fallible 1R 路径产出。1R 前尚无快照时可保持原 deferred 收集行为，但这不是 ready 或可启动凭据；闸门成功后必须让实际首个 Reason 看见 prepared 的完整结果。

5. **不能遗漏的时间顺序依赖：**当前 `build_session_tool_view` / catalog 构建早于 1R（`stage_builder.rs:456-466`）。仅在 `before_agent` 缓存 prepared、然后期望 collect_tools 自动重跑是错误方案；仅改 local_tools 也会被 Reason 的 refresh/map swap 覆盖（`reason.rs:23-31`）。B 必须在其方案中给出准入后重建/发布真实 session catalog 的宿主接线，保证校验快照、generation、实际工具视图一致，并重新应用 allow/disallow policy；不得绕过 policy 或覆盖 dynamic capability。**该宿主发布点超出 C 所有权，是 B/父计划的实施前阻塞项；若 B 也无此文件权限，由父计划先分配，不得擅自扩写 C。**

6. typed 快照必须对应本次 ready 的 handle generation。B 若检测到重连/快照变化，应重新准入、重新构建并原子替换，不能把旧 required 标记套到新 handle。普通 MCP 不应因该缓存永久失去原有每 turn 收集机会：B 在后续 turn 收集时重新取得当次静态快照并按同一闸门/校验流程发布；不要冻结整个 deployment 工具目录到会话结束。

7. 空 `required[server]` 也必须由 B 等待 ready；C 无法从零匹配集合证明 server 存在。成功后 collect 的 MCP Vec 与基线相同，没有多一份工具、没有把整个 server 设为 direct。

## 4. 任务表

实施顺序为 C-INJ-01 → C-INJ-02 → B 接线后的 C-INJ-03 验证。不同任务没有文件写入重叠；可独立验证指在所列前置依赖完成后单独运行该任务的目标测试，不要求依赖倒置。

| Task ID | 标题 | 目标文件 | 改动摘要 | 验证命令 | 预估 diff 规模 | 文件冲突面 |
| --- | --- | --- | --- | --- | --- | --- |
| C-INJ-01 | 支持 typed bridge 的 direct 提升 | `peri-middlewares/src/mcp/tool_bridge.rs` | direct 默认 false、override、with_direct、原始名 accessor、Clone；提取 typed builder，public builder 保持兼容；同文件新增不足 30 行的 focused test | `cargo test -p peri-middlewares --lib mcp::tool_bridge` | 约 50–90 行 | 仅 C；不改原 `tool_bridge_test.rs`、middleware 或动态调用方 |
| C-INJ-02 | 解析、批量验证及真实分类 seam 测试 | 新 `peri-middlewares/src/mcp/system_tools.rs`、新 `peri-middlewares/src/mcp/system_tools_test.rs`、`peri-middlewares/src/mcp/mod.rs` 仅一行 | 实现冻结函数/错误、schema 结构检查；本地 fixture；通过真实 catalog、ToolSearch hook、run_reason 验证模型工具视图；测试模块声明随主模块一并落地 | `cargo test -p peri-middlewares --lib mcp::system_tools` | 约 500–750 行（多数为 fixture/测试） | 新模块归 C；mod.rs 该行需父计划协调；不改 B 文件 |
| C-INJ-03 | 验收接线与首 Reason 回归 | 无写入；只读 B/父计划交付物 | 检查 prepare 原子性、collect 替换而非 append、真实 view 发布、空数组仍等待；复跑目标集与历史回归 | `cargo test -p peri-middlewares --lib mcp::system_tools`；`cargo test -p peri-middlewares --lib tool_search`；`cargo test -p peri-agent --lib session::exec`；B 的目标测试过滤器 | 0 行 | 不领取或修改 B 的文件；失败退回具体 owner 修复 |

C-INJ-02 若实现过程中扩展成通用 schema 引擎，应停止拆解范围，不以“顺便完善”为由突破文件所有权。测试辅助代码按 `docs/standards/testing.md:14-19` 放本地 `_test.rs`，不新增共享 test_helpers。

## 5. 验证计划

### 5.1 C 的确定性无服务器测试

以下除首项外均放 `peri-middlewares/src/mcp/system_tools_test.rs`，不需要真实 MCP server，不访问网络。fixture 仿照 `tool_bridge_test.rs:4-31` 构造真实 rmcp Tool、真实 McpToolBridge，绝不只用 MockTool 代替被测 bridge。

| 测试函数名 | seam / 可观察断言 |
| --- | --- |
| `test_system_direct_flag_preserves_bridge_identity`（`tool_bridge.rs` 新的小型内联测试模块） | new 默认 false；with_direct 后 true，name、raw tool name、mcp_server_name、parameters、visible_to_model 不变 |
| `test_system_required_tools_resolve_in_own_namespace` | prepare 输入含 workspace/Read 和 archive/Read；仅 workspace 项被提升；返回 effective 名正确，Vec 数量和身份不变 |
| `test_system_required_tool_names_are_case_sensitive` | workspace/read 不满足 Read；返回 MissingTool 的 server/tool 精确对应配置 |
| `test_system_effective_prefix_is_not_stripped` | 仅有 raw Read 时，配置 mcp__workspace__Read 返回 MissingTool，不偷用 effective name 搜索 |
| `test_system_missing_tool_returns_explicit_error` | prepare 返回 MissingTool，不是空 Ok；另一 server 有同名工具也不能补足 |
| `test_system_invalid_schema_returns_explicit_error` | Tool 反序列化成功后，properties=42 / required 非字符串数组 / 嵌套非法 type 等表驱动输入均返回 InvalidSchema；断言安全路径、固定原因，无 schema 值泄漏 |
| `test_system_valid_schema_preserves_extensions_and_data` | object、嵌套 schema、boolean schema 节点、vendor extension、default/enum 中普通数据合法；parameters 完整保留，不改写 MCP 原始 schema |
| `test_system_empty_required_array_adds_no_direct_tools` | required 含 workspace:[]；prepare 成功，输入/输出工具数与定义一致、direct 增量为 0；普通 deferred schema 不触发必需检查；不把此测试称为 ready 测试 |
| `test_system_duplicate_requirements_do_not_duplicate_registration` | [Read,Read] 只提升已有一项；断言 prepare Vec 中 effective name 计数为 1，且 catalog map 仍为 1；不能只看 map 长度掩盖重复 Vec |
| `test_system_ambiguous_raw_tool_is_rejected` | 同一 server 两份 raw Read → AmbiguousTool，不任意取第一项 |
| `test_system_effective_name_collision_is_rejected` | required 命中项与不同原始名/不同 server 的 sanitized effective 名碰撞，或 ASCII case-fold 冲突 → EffectiveNameCollision |
| `test_system_app_only_required_tool_is_rejected` | _meta 将工具设为 app-only → NotModelVisible，禁止强制 model visibility |
| `test_system_validation_is_all_or_nothing` | [合法 Read, 非法 Write] → Err，无可发布的部分 Vec；测试 B 之前不宣称 ready 原子性已被验证 |
| `test_system_required_tools_reach_first_reason_without_search` | prepare → `SessionToolCatalog::try_new` →真实 `run_reason`，记录型 ReactLLM 捕获实际 tools 入参；mcp__workspace__Read 恰好一次、is_direct=true、schema 等于原声明，无需调用 SearchExtraTools；ReasonOutput.catalog.direct_definitions 同样包含它 |
| `test_system_ordinary_tools_remain_deferred_through_tool_search` | 同批 required Read、未选中 Glob、普通 MCP 工具；执行真实 before_agent/before_reason_catalog；index.get_tool(Read)=None，Glob/普通工具=Some；run_reason 的 LLM tools 不含这些 deferred，SearchExtraTools 仍可发现它们 |
| `test_system_direct_tool_survives_tool_search_rebind` | 重复调用 before_reason_catalog，比较 working map 中 direct bridge 标志/身份及 pinned direct_definitions；ToolSearch 不覆盖或重新索引该工具 |
| `test_system_dynamic_control_remains_deferred` | new_dynamic / DynamicMCP 控制工具不因新增 direct 字段而被自动提升；动态 gate 路径保持原样 |

实际 seam 构造说明：

- `SessionToolCatalog::try_new`、`snapshot`、`pin_working_tools` 是可访问的真实产物（`peri-agent/src/session/tool_catalog.rs:114-119,177-178,201-224`），测试使用其输出而非重写分类函数。
- `run_reason` 为 public（`peri-agent/src/agent/stages/reason.rs:14`，模块导出见 `stages/mod.rs:13`）；记录型 LLM 实现真实 `ReactLLM::generate_reasoning` 接口（`peri-agent/src/agent/react.rs:263-269`）。`StageContext::builder` fixture 模式见 `peri-agent/src/agent/stages/reason_test.rs:13-18,32-38`；只替换 LLM 外部边界，不 mock catalog/ToolSearch。
- ToolSearch local_tools fixture 模式见 `peri-middlewares/src/tool_search/middleware_test.rs:192-228`；新测试可用更小的真实 CatalogState 实现，不复制整个测试文件。索引可观察 API 在 `tool_index.rs:289-290,349-350`。
- C 测试可证明 prepare 输出经过真实 Reason 分类，但不能从另一个 crate 直接调用 private `build_session_tool_view`，也不能证明 B 的 1R 自动更新了生产视图。

### 5.2 必须由 B/父计划补齐的生产装配断言

以下是交接的测试需求，**不作为 C 领取文件的任务**。建议由 B 在其拥有的 `peri-middlewares/src/mcp/middleware_test.rs` 编写 gate 测试；跨 Agent 私有 seam 的测试由父计划指定 owner 在 `peri-agent/src/session/exec/stage_builder/builder_v2_test.rs` 中实现，不擅自修改其权限。

- `test_system_gate_missing_required_tool_blocks_first_reason`：真实 B gate 收到 C MissingTool，返回 RequiredTools 包装错误；ready 未发布，记录型 LLM 调用次数为 0。
- `test_system_gate_invalid_schema_blocks_first_reason`：同上，错误类别为 InvalidSchema，不把 schema 错误降为工具搜索不可用。
- `test_system_gate_empty_required_array_waits_for_discovery`：空数组且 discovery pending 时不得通过；成功后 direct 增量 0；initialize/list 失败或 timeout 时仍 Err。可用内存 transport fixture，不能用 peer=None 伪造握手完成。
- `test_system_ready_refreshes_initial_session_tool_view`：初始 collect 时工具未 ready，1R 后完成协议与发现；断言由真实 `build_session_tool_view` / catalog 发布链送到**首个 Reason**的工具集合含 required 且无重复，deferred 路径仍有效。禁止只手工调用第二次 collect 来替代生产发布验证。
- 同一组测试断言 gate 失败不会留下可被下次启动复用的半成品 direct 快照。

验收 3 完成条件是 C 的正/负例、真实 Reason 入参、B 的首 Reason 接线三个层面均通过；验收 4 还必须有 B 的 ready/timeout 测试。只通过 C 的无网络测试不等于验收完成，更不等于五个 MCP 迁移完成。

## 6. 风险与未知

1. **重复注册与有效名碰撞：已查明，不依赖猜测。**现有 Vec 不去重、session map 后写覆盖。采用单次 typed 构建、原对象标记、整体替换初始 Vec；required 涉及的净化/大小写碰撞 fail closed。绝不 append 第二份 direct bridge。
2. **1R 与工具视图的时间错位是阻塞项。**B 必须明确在实际宿主 seam 发布 prepared；当前 cache-only / collect-only 接线不足以完成验收。本文没有替 B 发明一个现存的 refresh API，也不越权新增它。父计划未分配所需宿主接线前，不可宣称本计划已经端到端可实施完成。
3. **Dynamic session-scoped registry：现有行为必须保留。**同 server 动态 projection 会整组 shadow static，即使 static 已 direct。C 不使 new_dynamic 默认 direct，不改 registry、不泄漏其它 session 的 client/lease，也不通过静态缓存覆盖动态实例。对于“已激活的动态 server 与 System MCP 同名”，设计未规定优先级；父计划/B 必须在准入时解决有效来源一致性（建议明确报冲突并阻止本次启动，而不是默默用静态 ready 证明动态能力）。错误产生在 B 的有效来源准入层，不由缺少 session capability 的纯函数凭空判断。运行中是否允许后来 shadow 必需工具，亦需父计划明确；不能借本次工作私自改变动态 load/unload 语义。
4. **Schema 保证的范围尚无统一 draft 事实源。**typed JSON 可序列化不等于 JSON Schema 有效。本文冻结最小结构解析并覆盖可构造的非法 fixture，不宣称完整 draft 编译。若需要完整 meta-schema 验证或 `$ref` 可解析性，这是待父计划确认的材料性要求，涉及新增依赖/manifest 所有权；不允许悄悄新增 library、联网解引用或仅用 roundtrip 冒充验证。
5. **app-only 与 policy 过滤。**必需项不能强行覆盖 model visibility；effective allow/disallow policy 仍有效。若 policy 排除了必需 direct 工具，B/父计划必须在实际准入视图明确处理不满足启动要求，而不是 C 绕过权限过滤。正常注入测试必须使用允许该工具的 policy。
6. **连接 ready 的证据：**initialize 当前有 tools/list 吞错分支；空数组尤其暴露该问题。B 需保证成功 evidence，不以 get_all_clients 的 Connected 过滤代替协议完成证明。B 的错误类型及字段实际代码尚未落地，RequiredTools 变体为此处的冻结交接要求。
7. **快照与新鲜度：**prepared clone 会复制 schema/description，但复用 client/gate/leases；B 不应跨 generation 盲目复用。普通 deferred 来源的更新机会不可丢失，不能把此处“同一次准入快照”误实现为永不刷新的 session 全局表。
8. **Prompt 体积是设计输入。**direct 工具每次 Reason 携带完整 schema/description，成本近似为必需工具声明序列化大小之和；根 `CLAUDE.md:15` 明确成本约束。只提升显式数组，空数组为零增量；不默认整台 System MCP 全量 direct、不再把 required schema 拼一份进搜索提示词。验证时记录 direct 数量与序列化字节增量（不输出内容/秘密），不编造 token 数，不设置设计未授权的硬上限，也不以预算为由静默截掉必需工具。
9. **验证边界：**本次只做 Read/Grep/Glob 侦察及本文写入，没有编译或运行时结果；行号是侦察时快照，并行实现后应按符号复核。B/父计划的宿主接线、动态冲突决策、完整 schema draft 要求均未能由当前代码证明，须在实施前闭合。

## 7. 非目标

- 不实施 Workspace/Artifact/Web/Cron/LSP 五个实例的真实迁移、拆包、进程化或 capability root 隔离改造。
- 不领取验收 5、6 的实现范围；仍不得破坏已有 Permission/HITL/cancel/effective name/session 契约。
- 不改 MCP transport、OAuth、重试、timeout、协议版本协商或 B 的 ready 状态机。
- 不把普通 deferred 工具改成 direct，不更换 ToolSearch 索引或搜索算法，不新增裸工具名 alias。
- 不改变动态 MCP registry 的 session 隔离、projection lease、admission 或 shadow 架构。
- 不实现完整 JSON Schema 执行验证器、远程引用加载或 provider-specific schema 降级。
- 不增加全局 registry 写入、不以修改 shared_tools 绕过 session-local 事实源。
- 不修改本文件之外的源码、测试、配置或文档，不提交 git，不运行 cargo build/test；本文描述的实现与验证均是未来工作。
