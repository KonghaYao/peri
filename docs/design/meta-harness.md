# MetaHarness 设计 — 提示词段落替换与 middleware 卸载

> 状态：现行设计
>
> 本文是 MetaHarness 机制与设计决策的单一事实源。代码清单与入口以
> `peri-acp-types/src/meta_harness.rs`、`peri-middlewares/src/assembly.rs` 和相邻测试
> 为准；冻结、能力闭包与缓存边界分别服从 ARC-FROZEN-001、
> ARC-CAPABILITY-CLOSURE-001 与 ARC-SERIAL-001。

## 1. 目标与使用场景

MetaHarness 一个 kv 字段承载三项能力，key 类型决定动作：

| key 类型 | value | 动作 |
| --- | --- | --- |
| 段落 ID | `true` | **覆盖系统提示词**（第一能力） |
| middleware 名 / builtin 实例策略键 | `false` | **关闭能力**（第二能力）：链槽位名 ⇒ 该 middleware 不进链；builtin 实例策略键（`WebMiddleware` / `ArtifactMiddleware` / `CronMiddleware` / `LspMiddleware` / `WorkspaceMiddleware`）⇒ 该实例的工具面关闭 |
| `BuiltInSubagents` | `true` / `false` | 启用 / 屏蔽 compile-time built-in subagent definitions（默认启用） |

`BuiltInSubagents` 只控制 built-in definition provider，不关闭 `SubAgentMiddleware`；
项目级及 plugin agents、`fork`、`resume`、`Agent` / `AgentResult` 工具保持可用。
该值在 `session/new` 时随 `MetaHarnessState` 冻结，available agents catalog 与
新建 named subagent 的 definition fallback 共同遵守。
### 场景 1：覆盖系统提示词段落

用户要完全替换系统提示词的 `01_intro`（角色定义）与 `05_using_tools`（工具
使用策略）段落：

```
.peri/meta/01_intro.md            # 新角色定义（md 全文 = 替换体）
.peri/meta/05_using_tools.md      # 新工具策略
settings.json:
{ "meta_harness": { "01_intro": true, "05_using_tools": true } }
```

期望：`session/new` 渲染系统提示词时，这两个段落内容被 md 全文替换；段落
渲染顺序（按位置属性 + 段内序号）不变；其余段落保持内置。

### 场景 2：关闭能力（卸载工具）

用户要关闭 Web 工具：

```
settings.json:
{ "meta_harness": { "WebMiddleware": false } }
```

期望：`WebMiddleware` 是 **builtin `web` 实例**的关闭键（v4-part-2 起 Web /
Artifact 已不是链槽位；键集合 = `MIDDLEWARE_NAMES` ∪
`BUILTIN_INSTANCE_POLICY_KEYS`，两键仍被识别为已知键），
`WebSearch` / `WebFetch` 从 direct tools、deferred 目录与
检索、subagent `parent_tools`、workflow agent 工具列表四个面一并消失。关闭
**链槽位名**（如 `TodoMiddleware`）则是装配期该 middleware 不进链，其工具与
钩子全部失效（无需 md 文件）。

### 场景 3：回退与生效时机

删除 md / 删除 key → 下次会话创建生效（ARC-FROZEN-001：会话内冻结，不中途
重读）。

---

# 第一部分：理想架构

## 2. 目标态设计（实现以此为准）

### 2.1 配置面：settings.meta_harness

`AppConfig` 新增一个 kv 字段（与 `persona`/`tone` 同款 serde 风格）：

```rust
/// MetaHarness 控制字段：段落 ID → true（覆盖系统提示词段落）；
/// middleware 名 → false（装配期关闭该 middleware）；
/// BuiltInSubagents → bool（是否注册 compile-time built-in definitions，默认 true）
#[serde(default, skip_serializing_if = "Option::is_none")]
pub meta_harness: Option<HashMap<String, bool>>,
```

**双向语义**（bool 对两类 key 均有意义，无死角）：

| key 类型 | `true` | `false` |
| --- | --- | --- |
| 段落 ID | 覆盖该段落（需 `.peri/meta/<ID>.md` 存在） | 显式不覆盖（用内置段落） |
| middleware 名 | 显式恢复装配（覆盖全局的关闭） | 关闭该 middleware |
| `BuiltInSubagents` | 启用 compile-time built-in definitions | 不加入 catalog，且新建 named subagent 不回退到 built-in definition |

**合并语义**：**逐 key 合并**——项目级 `{cwd}/.peri/settings.json` 的 key
覆盖全局同 key，全局其余 key 保留。这是 **meta_harness 专属特例**，实现为 merge 内特例分支 +
测试锁定。本期**不提供 null/删除语义**（无法从项目级移除全局 key 本身，
需改全局配置）。

**校验**（解析时，warn 不 fail；**只校验 key 集合与值语义，不查文档**——
文档存在性校验在冻结期，见 2.3，避免解析期二次读盘；**非 bool 值保持
serde 类型错误 fail**——"warn 不 fail"仅适用于成功解析后的未知 key）：

合法键集合由 `peri-acp-types/src/meta_harness.rs` 的 `SECTION_IDS`、
`MIDDLEWARE_NAMES`、`BUILTIN_INSTANCE_POLICY_KEYS` 与 `BUILT_IN_SUBAGENTS_KEY`
共同定义；校验实现为 `AppConfig::validate_meta_harness`，设计不复制校验代码。
段落的 `false` 与链槽位/实例策略的 `true` 都是合法的显式恢复值。

### 2.2 段落覆盖来源：workspace `peri-meta://`（原宿主加载器已删除）

原 `peri-middlewares/src/meta_harness/` 加载器（`scan_harness_docs`）**已删除（W3b/J6）**：
段落覆盖文档改由 builtin `workspace` 实例的 `peri-meta://workspace/{section_id}` 资源提供
（provider 侧扫描，见 `mcp-packages/workspace/src/resources/meta.rs`；URI 形状与解析契约在
`peri-acp-types/src/workspace_resources.rs`；宿主消费在
`peri-middlewares/src/mcp/client.rs::read_builtin_workspace_meta`）：

- **来源白名单（X7）**：只消费 host 绑定为真实 builtin `workspace` 实例的句柄
  （`ConfigSource::Builtin { instance: "workspace" }` 且 `Connected`）；外部 origin 的同 scheme
  资源一律拒绝并记录，判定依据是实例身份而非资源文本自称。
- **关闭与失败（X8）**：覆盖不可得（实例关闭 / 文档缺失 / 读取失败）⇒ warn 并保持内置段落、
  不阻塞会话创建、**不回落磁盘**；必选输入的读取失败仍按其契约走发布前失败补偿，legacy 首次接纳
  无执行环境 ⇒ 覆盖不可得（保持内置）。
- **既有扫描语义**（今在 provider 侧）：仅一级目录 `{cwd}/.peri/meta/*.md`（不递归）；非 `.md`
  文件忽略；文件 stem 即 section_id（`"01_intro.md"` → `"01_intro"`）；读取失败跳过该文件、
  不 fail 扫描。
- **排查指引（scanner 删除后）**：配置了 `"<section>": true` 而覆盖未生效时，依次检查
  ① `.peri/meta/<section>.md` 是否在 workspace 根下且可读；② `workspace` 实例是否被关闭
  （`"WorkspaceMiddleware": false` / 实例 `disabled` / 进程级开关）——关闭即无覆盖来源；
  ③ 进程日志中 provider 的缺失/读取失败 warn 与宿主身份过滤记录。

### 2.3 冻结状态：MetaHarnessState

契约类型唯一声明在 `peri-acp-types/src/meta_harness.rs::MetaHarnessState`：
冻结覆盖正文、关闭的链槽位/builtin 策略键，以及内置 SubAgent 定义开关。
本文不复制结构字段，避免合法键与冻结状态漂移。

- **构建时点**：`build_frozen_data` 冻结期、渲染 system prompt 之前——一次
  读取 settings +（J6 后）workspace `peri-meta://` 资源读取（宿主 scanner 已删除）。
- **文档存在性校验在此处**：开关 `true` 但资源面无对应文档 → warn + 忽略该
  条目（保持内置段落），不二次读取、也不回落磁盘。
- **挂载要求**：`FrozenContext` 单份存储 `meta_harness`
  字段，`FrozenSessionData` 经委托字段提供 accessor，`from_frozen_parts`
  不加重复参数，避免双事实源。
- **消费方统一冻结状态**：`disabled_middlewares` 首次
  session/new 构建进冻结状态后，**全部装配入口统一消费冻结副本**——主链
  每 turn 重新装配只复用，不再直读可变 `peri_config`（否则配置变更会在
  会话中途生效，违背 ARC-FROZEN-001）；SubAgent/fork 经冻结状态传播。

### 2.4 段落覆盖

段落声明的唯一来源是已装配 middleware 的 `PromptSection`；ID 对应模板名或
渲染生成段，位置与缓存语义由声明的 zone/order 决定，不另建 Layer。
channel 退役后不保留无持有者的内置段落数组或 feature gate。

**覆盖注入方式：PromptTemplate 构造期合并**：`new(state, collected)` 按 ID
替换已收集段落的内容，过滤空正文并排序，物化为 cached/uncached 两个容器。
不保留第二份覆盖 map；`render(env, agent_catalog)` 只拼接内容及替换环境占位符，
不读盘、不重新加载配置。内容源为 builtin 静态文本、冻结的覆盖正文或 middleware
动态正文；覆盖不能创建未装配持有者的段落。

**同源一致性要求**：所有 PromptTemplate 构造点统一从冻结载体取
MetaHarnessState 传入——冻结渲染与后续重渲染必须同一覆盖源，禁止出现
"冻结已覆盖、重渲染无覆盖"双轨不一致（现状构造点清单见 3.4）。

**边界**：

- **功能段落**：覆盖只改内容来源，是否装配持有 middleware 的判定不变；未装配功能的
  段落即使被覆盖也不渲染。
- **缓存区 transport seam**：现状 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 由
  `PromptTemplate::render` 生成，不在段落文件内；段落位置属性（契约 2）负责
  装配，保留控制字负责把 seam 跨越 `String` handoff 传给 provider。provider
  必须消费该控制字，wire prompt 不得包含它（ARC-SERIAL-001）。
- **冻结语义**：覆盖在冻结期构造时一次应用，产出后即 frozen，无运行时
  开销、无中途重读（ARC-FROZEN-001）。

### 2.5 middleware 关闭

**配置来源**：`AppConfig.meta_harness` 的 false 条目 → `disabled` 集合
（`AssemblyContext` 新增 `meta_harness_disabled: HashSet<String>` 字段透传）。

**关闭面 = 全部装配入口**（禁止只过滤顶层链——否则产生系统性链下泄漏：
parent_tools / Workflow agent 链 / 子链独立装配、无条件注入工具，关闭
Todo / SubAgent / Mcp 后子 agent 仍携带这些工具）：

```rust
// 每个装配入口内：
let disabled: HashSet<String> = meta_harness.iter()
    .filter(|(_, v)| !**v)
    .map(|(k, _)| k.clone())
    .collect();

if !disabled.contains("TodoMiddleware") {
    chain.add(Box::new(TodoMiddleware::new(...)));   // 关闭 → 不构造、不进链
}

// builtin MCP 实例（`WebMiddleware` / `ArtifactMiddleware` / `CronMiddleware` /
// `LspMiddleware` / `WorkspaceMiddleware`）不走链构造：它们由关闭集映射为实例关闭集，
// 再交给工具面过滤（`assembly.rs`）。
let closed_instances = crate::mcp::builtin::closed_instances(&disabled);
```

**联动清理**：关闭 SubAgentMiddleware 时，其关联构造（parent_tools 注入、
subagent_mw 槽位）联动置空，禁止半开状态。

**语义**：

- **关闭面 = 能力提供者**：链槽位 middleware 以 key = `name()` 返回值关闭；builtin
  MCP 实例以策略键（`BUILTIN_INSTANCE_POLICY_KEYS`，映射唯一来源是声明表的
  `policy_key`）关闭。同一提供者的全部工具随之一并关闭（连坐语义）。
- 关闭后该 middleware 的钩子（before_agent / before_tool /
  prompt_contribution / first_turn_reminder）全部不执行——工具与提示词贡献
  同时消失。
- **段落所有权跟随关闭面**：middleware 缺席时，其持有的 gated 段落与
  request-time contribution 同时消失；无持有者兼容段只按显式 gate 判定。
- **工具视图每 turn 重建**：禁用效果经
  `build_session_tool_view`（每 turn 从基础 shared_tools + 当前链工具重建
  session-local 视图）实现，**不修改宿主持有的全局共享表**（并发会话配置
  不同，改全局表不安全）；禁用 middleware 的工具天然不在视图内。
- **审批与提问分属独立关闭面**：`PermissionMiddleware` 持有审批钩子与
  `10_hitl`；`HumanInTheLoopMiddleware` 通过 `collect_tools()` 持有
  `AskUserQuestion` 与 `12_ask_user`。任一 middleware 关闭时，对应工具、钩子和
  prompt contribution 同时从 session-local 视图消失（见 ARC-CAPABILITY-CLOSURE-001）。
- 插件无独立 meta_harness 条目：关闭 `PluginMiddleware` 即关闭插件整体注入；
  插件卸载/管理走既有机制。
- Artifact 上传由 builtin `artifact` MCP 实例承载（`mcp-packages/artifact/src/{server,tool}.rs`；宿主实例注册与 dispatch 仍在 `peri-middlewares/src/mcp/builtin/`）；
  策略键 `ArtifactMiddleware: false` 关闭该实例的工具面，仅移除 `artifact`（模型面
  `artifact`），不影响 `ToolSearch` 的 `SearchExtraTools` / `ExecuteExtraTool`。
- `CronMiddleware` / `LspMiddleware` 是 `cron` / `lsp` 实例的策略键，语义与
  `ArtifactMiddleware` 同构，但两个实例的工具都是 deferred：关闭只收缩 deferred 目录与
  检索结果（`parent_tools` 与 workflow 工具列表本来就不含 deferred 工具）。LSP 另有一个
  **链槽位名** `LspSyncMiddleware`：关它只停文档同步，`mcp__lsp__LSP` 仍可见；`LspMiddleware`
  关闭则同时关工具面与同步目标。两者都保留实例、handler、pool 与 readiness。
- `WorkspaceMiddleware` 是 `workspace` 实例的策略键（v4-part-4 wave 3）：7 个文件/终端
  工具（`Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations` /
  `Bash`）在声明表中**全部**标为 direct，因此关闭必须在同一 turn 同时收缩三个真实面——
  首个模型请求的 tools、subagent `parent_tools` 与 workflow agent 工具列表（deferred 目录
  与检索面本来就不含 direct 工具，属平凡成立）。关闭后 peri 不再提供任何文件系统与
  shell 工具（`FilesystemMiddleware` / `TerminalMiddleware` 与 `ChainSlot::Filesystem` /
  `ChainSlot::Terminal` 已随本波删除，没有回退的 middleware 提供面），实例、handler 与
  pool 级连接状态仍保留。

### 2.6 生命周期

- 配置源：`AppConfig.meta_harness`（合并后，项目级覆盖全局）。
- 段落覆盖：`build_frozen_data` 冻结期一次应用。
- middleware 关闭：首次 session/new 构建进冻结状态，**全部装配入口统一消费
  冻结副本**——会话内每 turn 复用，不直读可变 `peri_config`。
- 变更（md / settings）下次会话生效；会话内与 SubAgent/fork 复用冻结状态。

### 2.7 数据结构与模块归属

| 组件 | 落点 | 说明 |
| --- | --- | --- |
| `MetaHarnessState` 类型 | `peri-acp-types/src/meta_harness.rs` | 跨层冻结载体，简单类型 |
| ~~`scan_harness_docs` 加载器~~ | ~~`peri-middlewares/src/meta_harness/`~~ | **已删除（W3b/J6）**：读取改由 workspace 实例的 `peri-meta://` resources 承担（`read_builtin_workspace_meta`） |
| settings 字段解析/合并 | `peri-acp/src/provider/config.rs` | `AppConfig.meta_harness` + merge 特例 + 校验 |
| 冻结组装 | `peri-acp/src/session/mod.rs` | build_frozen_data + 冻结载体三结构加字段 |
| 段落覆盖合并 | `peri-acp/src/prompt/mod.rs` | 数组加 ID（二元组/三元组，Layer 已去除）+ `PromptTemplate::new` 构造期合并（`SectionContent` 零拷贝双态） |
| 装配期过滤 | `peri-middlewares/src/assembly.rs` | disabled 集合裁剪链 + 全装配入口联动 |
