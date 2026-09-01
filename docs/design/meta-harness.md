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
| middleware 名 | `false` | **关闭 middleware**（第二能力） |
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

### 场景 2：关闭 middleware（卸载工具）

用户要关闭 Web 工具：

```
settings.json:
{ "meta_harness": { "WebMiddleware": false } }
```

期望：装配期 WebMiddleware 不进链，WebFetch / WebSearch 不进入工具列表，
其钩子全部失效（无需 md 文件）。

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

```rust
for (key, v) in meta_harness {
    match (key, v) {
        (k, _) if !SECTION_IDS.contains(k) && !MIDDLEWARE_NAMES.contains(k)
            => warn!("meta_harness: unknown key {k}"), // 忽略
        (k, false) if SECTION_IDS.contains(k) => { /* 段落显式不覆盖：合法，静默 */ }
        (k, true)  if MIDDLEWARE_NAMES.contains(k) => { /* 显式恢复装配：合法，静默 */ }
        _ => {}
    }
}
```

`SECTION_IDS` / `MIDDLEWARE_NAMES` 为编译期常量：段落文件名清单 +
装配面 middleware name 清单。

### 2.2 加载器：`.peri/meta/`

新模块 `peri-middlewares/src/meta_harness/`（与 `skills/`、`agents_md/`
同构：加载逻辑在 middlewares，类型归契约层）：

```rust
/// 扫描 {cwd}/.peri/meta/*.md，返回 文件名(去 .md) → 全文
pub fn scan_harness_docs(cwd: &str) -> HashMap<String, String>
// 规则：
//  - 仅扫描一级目录 *.md（不递归）；非 .md 文件忽略
//  - 文件名即 key（"01_intro.md" → "01_intro"）
//  - 读取失败（IO/权限）→ warn + 跳过该文件，不 fail 扫描
```

### 2.3 冻结状态：MetaHarnessState

```rust
/// 冻结期构建，随冻结载体传播；会话内不可变
pub struct MetaHarnessState {
    /// 段落 ID → md 全文；仅含"开关 true 且文档存在"的条目
    pub section_overrides: HashMap<String, Arc<str>>,
    /// 装配期关闭的 middleware 名集合（v=false 条目）
    pub disabled_middlewares: HashSet<String>,
}
```

- **构建时点**：`build_frozen_data` 冻结期、渲染 system prompt 之前——一次
  读取 settings + `scan_harness_docs`。
- **文档存在性校验在此处**：开关 `true` 但扫描无对应文件 → warn + 忽略该
  条目（保持内置段落），不二次读盘。
- **挂载要求**：`FrozenContext` 单份存储 `meta_harness`
  字段，`FrozenSessionData` 经委托字段提供 accessor，`from_frozen_parts`
  不加重复参数，避免双事实源。
- **消费方统一冻结状态**：`disabled_middlewares` 首次
  session/new 构建进冻结状态后，**全部装配入口统一消费冻结副本**——主链
  每 turn 重新装配只复用，不再直读可变 `peri_config`（否则配置变更会在
  会话中途生效，违背 ARC-FROZEN-001）；SubAgent/fork 经冻结状态传播。

### 2.4 段落覆盖

**数组升级**：系统提示词三个段落数组均携带 ID：
`IMMUTABLE_SECTIONS` / `ALWAYS_UNCACHED_SECTIONS` 为 "ID + 内容" 二元组；
`GATED_SECTIONS` 为 "ID + 内容 + Gate" 三元组。
ID 即 `prompts/sections/` 文件名去 `.md`（`01_intro`、`05_using_tools`…），
维护成本为零。

位置与缓存语义只由数组归属和段内序号决定，不另建内容分类 Layer。

**覆盖注入方式：PromptTemplate 构造期合并**（物化容器而非覆盖 map，内容源零拷贝双态）：
`PromptTemplate` 构造期按 ID 将段落内容替换为 md 全文后**物化为三个段落
容器**（`immutable_sections` / `always_uncached_sections` /
`gated_sections`，元素含 id/content），**不保留覆盖 map**（避免与
物化结果重复存储）；**render 签名与全部调用点不变**，改动收敛到 `new`
一处，render 零查表直接拼接：

```rust
// 内容来源双态：内置静态文本零拷贝借 &'static str，
// 覆盖全文持 Arc<str>——避免 Arc::from(builtin) 全量堆拷贝
enum SectionContent {
    Builtin(&'static str),   // include_str! 静态文本，零拷贝
    Override(Arc<str>),      // MetaHarness 覆盖全文（冻结期扫描）
}
struct ResolvedSection { id: &'static str, content: SectionContent }
// PromptTemplate::new(state: &MetaHarnessState) 内：
let immutable = IMMUTABLE_SECTIONS.map(|(id, content)| ResolvedSection {
    id,
    content: match state.section_overrides.get(id) {
        Some(overridden) => SectionContent::Override(Arc::clone(overridden)),
        None => SectionContent::Builtin(content),
    },
});
```
（`section_overrides` 字段仅存在于 `MetaHarnessState`，模板侧不重复持有。）

**同源一致性要求**：所有 PromptTemplate 构造点统一从冻结载体取
MetaHarnessState 传入——冻结渲染与后续重渲染必须同一覆盖源，禁止出现
"冻结已覆盖、重渲染无覆盖"双轨不一致（现状构造点清单见 3.4）。

**边界**：

- **gated 段落**：覆盖只改内容来源，`FeatureGate` 判定不变——未启用功能的
  段落即使被覆盖也不渲染（现状 gate 判定事实见 3.4）。
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
Filesystem/Web/Terminal/Mcp 后子 agent 仍携带这些工具）：

```rust
// 每个装配入口内：
let disabled: HashSet<&str> = meta_harness.iter()
    .filter(|(_, v)| !**v)
    .map(|(k, _)| k.as_str())
    .collect();

if !disabled.contains("WebMiddleware") {
    chain.add(Box::new(WebMiddleware::new(...)));   // 关闭 → 不构造、不进链
}
```

**联动清理**：关闭 SubAgentMiddleware 时，其关联构造（parent_tools 注入、
subagent_mw 槽位）联动置空，禁止半开状态。

**语义**：

- **关闭面 = middleware 实例**（key = `name()` 返回值）；同一 middleware 的
  全部工具随之一并关闭（连坐语义）。
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
- Artifact 上传由独立 `ArtifactMiddleware` 承载；关闭它仅移除 `artifact`，不影响
  `ToolSearch` 的 `SearchExtraTools` / `ExecuteExtraTool`。

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
| `scan_harness_docs` 加载器 | `peri-middlewares/src/meta_harness/` | 与 skills/agents_md 同构 |
| settings 字段解析/合并 | `peri-acp/src/provider/config.rs` | `AppConfig.meta_harness` + merge 特例 + 校验 |
| 冻结组装 | `peri-acp/src/session/mod.rs` | build_frozen_data + 冻结载体三结构加字段 |
| 段落覆盖合并 | `peri-acp/src/prompt/mod.rs` | 数组加 ID（二元组/三元组，Layer 已去除）+ `PromptTemplate::new` 构造期合并（`SectionContent` 零拷贝双态） |
| 装配期过滤 | `peri-middlewares/src/assembly.rs` | disabled 集合裁剪链 + 全装配入口联动 |
