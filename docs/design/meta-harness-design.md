# MetaHarness 设计 — 提示词完全替换与 middleware 卸载

> 日期：2026-08-14 | 修订：2026-08-14（结构重写：理想架构 → 现状）
> 状态：设计定案；波 1（段落覆盖）+ 波 2（middleware 关闭）已实现落地
> （2026-08-14，实现偏差裁定 Q1-Q10 已同步至 2.1/2.3/2.4/2.5/3.3/3.4；
> 2026-08-14 advisor 红队复审：Q1 推翻（PromptLayer 去除，兑现"层概念全部
> 去除"约束）、Q2 实现修正（SectionContent 零拷贝双态）、Q5 接线事实修正
> + 契约测试锁定、Q10 冻结时序证据成立——均已同步 2.4/3.1/3.3/2.7/2.8；
> 2026-08-14 波 3+波 4（wave3-4 workflow，C1-C5）已实现落地：段落持有者
> 基础设施 + 基础段/gated 段全部迁移至功能 middleware（gate = 持有者是否
> 在链上，契约 3）+ boundary 删除 + permission_mode_notice 删除 +
> 02/07/14/16 段落重组——3.1/3.4/3.5 已同步为现状，对账裁决见 3.5）

本文是 MetaHarness 机制的**单一事实源**。结构：第 2 节为**理想架构**（目标态
设计，实现以此为准，不夹杂现状细节）；第 3 节为**现状**（当前代码事实、理想
与现状的差距、演进方向）。外部跨模块契约（如 ARC-FROZEN-001）引用
`docs/standards/architecture-contracts.md`。

## 1. 目标与使用场景

MetaHarness 一个 kv 字段承载两项能力，bool 值决定动作：

| value | 动作 | key 语义 |
| --- | --- | --- |
| `true` | **覆盖系统提示词**（第一能力） | key = 段落 ID |
| `false` | **关闭 middleware**（第二能力） | key = middleware 名 |

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
/// middleware 名 → false（装配期关闭该 middleware）
#[serde(default, skip_serializing_if = "Option::is_none")]
pub meta_harness: Option<HashMap<String, bool>>,
```

**双向语义**（bool 对两类 key 均有意义，无死角）：

| key 类型 | `true` | `false` |
| --- | --- | --- |
| 段落 ID | 覆盖该段落（需 `.peri/meta/<ID>.md` 存在） | 显式不覆盖（用内置段落） |
| middleware 名 | 显式恢复装配（覆盖全局的关闭） | 关闭该 middleware |

**合并语义**：**逐 key 合并**——项目级 `{cwd}/.peri/settings.json` 的 key
覆盖全局同 key，全局其余 key 保留。这是 **meta_harness 专属特例**（现有
merge 对全部 Option 字段是"整体覆盖"，见 3.3），实现为 merge 内特例分支 +
测试锁定。本期**不提供 null/删除语义**（无法从项目级移除全局 key 本身，
需改全局配置）。

**校验**（解析时，warn 不 fail；**只校验 key 集合与值语义，不查文档**——
文档存在性校验在冻结期，见 2.3，避免解析期二次读盘；**非 bool 值保持
serde 类型错误 fail**——"warn 不 fail"仅适用于成功解析后的未知 key，
实现裁定 Q4）：

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
- **挂载要求（实现裁定 Q3）**：`FrozenContext` 单份存储 `meta_harness`
  字段，`FrozenSessionData` 经委托字段提供 accessor，`from_frozen_parts`
  不加重复参数（避免双事实源；现状与改动连锁见 3.4）。
- **消费方统一冻结状态（实现裁定 Q10）**：`disabled_middlewares` 首次
  session/new 构建进冻结状态后，**全部装配入口统一消费冻结副本**——主链
  每 turn 重新装配只复用，不再直读可变 `peri_config`（否则配置变更会在
  会话中途生效，违背 ARC-FROZEN-001）；SubAgent/fork 经冻结状态传播。

### 2.4 段落覆盖

**数组升级**：系统提示词三个段落数组均携带 ID：
`IMMUTABLE_SECTIONS` / `ALWAYS_UNCACHED_SECTIONS` 为 "ID + 内容" 二元组；
`GATED_SECTIONS` 为 "ID + 内容 + Gate" 三元组。
ID 即 `prompts/sections/` 文件名去 `.md`（`01_intro`、`05_using_tools`…），
维护成本为零。

> **实现裁定 Q1（advisor 复审后去除 Layer）**：早期实现曾保留 `PromptLayer`
> 内容分类枚举（SafetyAuthorization / EngineeringBehavior / CapabilityContract
> / RuntimeStateBoundary / PersonaDomain），并声称"Layer 是波 4 位置属性演进的
> 基础"。advisor 红队复核后推翻：Layer 不参与任何渲染逻辑（代码中为
> `#[allow(dead_code)]` 纯元数据），位置与缓存语义由数组归属承担；波 4 位置
> 属性（boundary 前/后 + 段内序号）与内容分类正交，不需要 Layer 打底。
> 且与"层概念全部去除"约束冲突。**已删除**：枚举、数组第三/四元素、
> `ResolvedSection.layer` 字段全部移除，数组降为二元组/三元组。

**覆盖注入方式：PromptTemplate 构造期合并**（不破 render 签名；实现裁定
Q2——物化容器而非覆盖 map，内容源零拷贝双态）：
`PromptTemplate` 构造期按 ID 将段落内容替换为 md 全文后**物化为三个段落
容器**（`immutable_sections` / `always_uncached_sections` /
`gated_sections`，元素含 id/content），**不保留覆盖 map**（避免与
物化结果重复存储）；**render 签名与全部调用点不变**，改动收敛到 `new`
一处，render 零查表直接拼接：

```rust
// 内容来源双态（实现裁定 Q2 复审）：内置静态文本零拷贝借 &'static str，
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
- **边界标记（演进后删除）**：现状 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__`
  由渲染逻辑生成，不在段落文件内；演进（3.5.2）后标记删除，缓存区划分由
  段落位置属性（契约 2）显式承担。
- **冻结语义**：覆盖在冻结期构造时一次应用，产出后即 frozen，无运行时
  开销、无中途重读（ARC-FROZEN-001）。

### 2.5 middleware 关闭

**配置来源**：`AppConfig.meta_harness` 的 false 条目 → `disabled` 集合
（`AssemblyContext` 新增 `meta_harness_disabled: HashSet<String>` 字段透传）。

**关闭面 = 全部装配入口**（禁止只过滤顶层链——否则产生系统性链下泄漏：
parent_tools / Workflow agent 链 / 子链独立装配、无条件注入工具，关闭
Filesystem/Web/Terminal/Mcp 后子 agent 仍携带这些工具。现状 5 处装配入口
清单见 3.4，含 /bg 后台 agent 的 `parent_tools_factory`——实现裁定 Q9）：

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
- **gated 段落不受关闭影响**：关闭不改变段落 gate 判定与段落内容（现状
  事实见 3.4）——段落内容的唯一清除手段是段落覆盖（2.4）或演进后的
  DefaultSystemPromptMiddleware（3.5）。
- **工具视图每 turn 重建（实现裁定 Q8）**：禁用效果经
  `build_session_tool_view`（每 turn 从基础 shared_tools + 当前链工具重建
  session-local 视图）实现，**不修改宿主持有的全局共享表**（并发会话配置
  不同，改全局表不安全）；禁用 middleware 的工具天然不在视图内。
- **AskUserQuestion 不在关闭面**：核心交互工具，非 middleware 提供
  （架构依据见 3.4）。
- 插件无独立 meta_harness 条目：关闭 `PluginMiddleware` 即关闭插件整体注入；
  插件卸载/管理走既有机制。

### 2.6 生命周期

- 配置源：`AppConfig.meta_harness`（合并后，项目级覆盖全局）。
- 段落覆盖：`build_frozen_data` 冻结期一次应用。
- middleware 关闭：首次 session/new 构建进冻结状态，**全部装配入口统一消费
  冻结副本**（Q10 裁定）——会话内每 turn 复用，不直读可变 `peri_config`。
- 变更（md / settings）下次会话生效；会话内与 SubAgent/fork 复用冻结状态。

### 2.7 数据结构与模块归属

| 组件 | 落点 | 说明 |
| --- | --- | --- |
| `MetaHarnessState` 类型 | `peri-acp-types`（新文件 `meta_harness.rs`） | 跨层冻结载体，简单类型 |
| `scan_harness_docs` 加载器 | `peri-middlewares/src/meta_harness/` | 与 skills/agents_md 同构 |
| settings 字段解析/合并 | `peri-acp/src/provider/config.rs` | `AppConfig.meta_harness` + merge 特例 + 校验 |
| 冻结组装 | `peri-acp/src/session/mod.rs` | build_frozen_data + 冻结载体三结构加字段 |
| 段落覆盖合并 | `peri-acp/src/prompt/mod.rs` | 数组加 ID（二元组/三元组，Layer 已去除）+ `PromptTemplate::new` 构造期合并（`SectionContent` 零拷贝双态） |
| 装配期过滤 | `peri-middlewares/src/assembly.rs` | disabled 集合裁剪链 + 全装配入口联动 |

### 2.8 测试计划

| 设计（2.x 节） | 测试 |
| --- | --- |
| 2.1 settings | `provider/config_test.rs`：字段解析、逐 key 合并（**专属特例**，与 env 整体覆盖行为对照）、布尔语义分流（true→段落 / false→middleware）、**双向语义**（段落 ID+false 显式不覆盖、middleware 名+true 显式恢复）、未知 key warn；`provider/store_test.rs`：**`load()` 生产接线契约**（advisor 复审后补：全局+工作区双文件经生产入口 `load()` 合并生效，逐 key 语义锁定） |
| 2.2 加载器 | `meta_harness/mod_test.rs`：扫描一级目录、文件名→key、非 md 忽略、IO 失败跳过 |
| 2.3 冻结 | `session/mod_test.rs`：冻结期构建一次（含 SubAgent 无 workflow 版渲染）、文档缺失 warn+忽略（冻结期校验，不二次读盘）、SubAgent/fork 复用 |
| 2.4 段落覆盖 | `prompt` 渲染测试：`PromptTemplate::new` 构造期合并后输出为 md 全文、段落顺序不变、gated 段落 gate 行为不变、未覆盖段落不受影响；**全部构造点同源一致性测试**——重渲染调用点与冻结渲染结果一致（防双轨不一致） |
| 2.5 关闭 | `assembly` 测试：关闭后 middleware 不进链、工具消失、钩子不执行、连坐语义；**5 处装配入口联动**（关闭 Filesystem/Web/Terminal/Mcp 后 SubAgent 与 workflow agent 不再携带其工具）、关闭 SubAgentMiddleware 时关联构造联动置空、session-local 工具视图隔离 |
| 2.4 边界 | 渲染结构不变测试；两动作独立生效测试（关 middleware 不影响段落覆盖）；卸载联动（配置移除后恢复） |

### 2.9 实施步骤

- **波 1**（段落覆盖，**已实施 2026-08-14**）：`AppConfig` 字段 + merge
  专属特例 + 解析期校验 → `scan_harness_docs` → `MetaHarnessState` + 冻结
  载体挂载（breaking 连锁同步）→ 数组加 ID + `PromptTemplate` 构造期物化
  合并 + 全部构造点同源注入 → 契约测试（2.1-2.4）。
  （2026-08-14 advisor 复审后：数组降为二元组/三元组，`PromptLayer` 去除、
  `SectionContent` 零拷贝双态——见 2.4。）
- **波 2**（middleware 关闭，**已实施 2026-08-14**）：`AssemblyContext`
  新增 disabled 字段透传 + 全部 5 处装配入口联动过滤 + 关联构造联动清理 +
  session-local 工具视图 → assembly 测试（2.5）。
- **波 3**：示例文档与安全段落覆盖风险提示（2.4 边界）。
- **波 4**（演进，见 3.5，**已实施 2026-08-14**，wave3-4 workflow C1-C5）：
  内置段落按 3.1.1 归属全景重构（07+14 合并、Proactiveness 并入 03、
  16_workflow 删除、sensitive 列表与 Selection Guide 改 middleware 动态
  生成）并收敛为 `DefaultSystemPromptMiddleware` 与各功能 middleware；渲染
  生成段重构（3.5.2）：boundary 删除、Persona 并入
  DefaultSystemPromptMiddleware、Language 新开 `LangMiddleware`；删除
  `permission_mode_notice` 运行时注入。演进后"完全纯净"路径闭环。

---

# 第二部分：现状

## 3. 当前代码事实与差距

### 3.1 系统提示词段落装配现状

**波 4 后（C2/C3 落地）**：段落来源 = middleware 收集结果（`build_collected_sections`
静态声明 / 链侧 `collect_prompt_sections` 收集，单一事实源禁止双轨）+
编译期内置数组仅剩无持有者的 `15_channel`。**ID 即
`peri-acp/prompts/sections/` 文件名去 `.md`**（含渲染生成段 `persona` /
`language`）；ID 是 MetaHarness 覆盖与持有权迁移的定位键，render 不依赖 ID。

| 来源 | 段落（ID） | 持有者 |
| --- | --- | --- |
| `GATED_SECTIONS` 数组（`prompt/mod.rs`，1 项） | 15_channel（gate 恒 false） | 无（未来 channel middleware；数组元组 ID + 内容 + Gate + 段内序号） |
| 收集结果（`DefaultSystemPromptMiddleware::sections`，`peri-middlewares/src/default_system_prompt/`） | 01_intro, 02_system, 03_doing_tasks, 04_actions, 05_using_tools, 06_tone_style（Builtin，Cached 1-6）+ 07_runtime（Builtin，Uncached 1）+ persona（Dynamic，Uncached 0） | DefaultSystemPromptMiddleware（关闭即清除全部基础段与 persona 覆盖） |
| 收集结果（`LangMiddleware::sections`） | language（Dynamic，Uncached 7） | LangMiddleware（可关闭、可覆盖） |
| 收集结果（`HumanInTheLoopMiddleware::sections`） | 10_hitl（Builtin，Uncached 3） | HumanInTheLoopMiddleware |
| 收集结果（`SubAgentMiddleware::sections`） | 11_subagent（Builtin，Uncached 4） | SubAgentMiddleware |
| 收集结果（`SkillsMiddleware::sections`） | 13_skills（Builtin，Uncached 5） | SkillsMiddleware |

`IMMUTABLE_SECTIONS` / `ALWAYS_UNCACHED_SECTIONS` 数组已删除（C2：迁移完成
禁止双轨）；`GATED_SECTIONS` 由 5 项收敛为 1 项（16_workflow 删除、gated
段随持有者迁移）。

渲染：`PromptTemplate::render`（`prompt/mod.rs:376-383`）按"位置属性 + 段内
序号"拼接（`new` 构造期物化 + stable 排序，契约 2）；段落仍会话级冻结
（`build_frozen_data`，`session/mod.rs:459`，**一次构造一次渲染**——
`subagent_system_prompt` 二次渲染已随 C5 移除，子面向复用主冻结 prompt）。
SubAgent/fork 复用。

### 3.1.1 系统提示词段完整清单与归属全景（现状构成 + 目标态）

**波 4 后现状（C2/C3 落地，2026-08-14）**：段落文件 11 个（01-06 /
07_runtime / 10_hitl / 11_subagent / 13_skills / 15_channel；07_env +
14_system_reminder 合并为 07_runtime，16_workflow 删除）+ 渲染生成段 2 个
（persona / language）。全部段落（除 15_channel）由功能 middleware 持有
（持有者表见 3.1 来源表；gated 段映射见 `SECTION_HOLDER_MIDDLEWARE`）——
gate = 持有者是否在链上（契约 3，收集即装配），middleware 关闭自动移除其
段落，段落关闭盲区已闭合。以下为波 4 前构成（归档）与目标态归属（已全部
落地，落地标注见表格末列）。

**静态段落（波 4 前，13 个文件，编译期 `include_str!` 嵌入，单一持有者
`prompt/mod.rs`；归档）**：

| ID | 主题 | FeatureGate | 动态依赖 |
| --- | --- | --- | --- |
| 01_intro | 角色定义 + 防御性安全 | — | — |
| 02_system | 惯例遵循 | — | — |
| 03_doing_tasks | 任务执行步骤 | — | — |
| 04_actions | 操作可逆性 | — | — |
| 05_using_tools | 工具使用纪律（批处理/Bash 纪律） | — | — |
| 06_tone_style | 语气与简洁性 | — | — |
| 07_env | 环境快照 | — | `{{cwd}}/{{is_git_repo}}/{{platform}}/{{os_version}}/{{date}}`（PromptEnv，`prompt/mod.rs:96-102`） |
| 14_system_reminder | system-reminder 语义 | — | — |
| 10_hitl | HITL 审批模式 + **sensitive 工具列表** | Hitl（权限模式 ≠ Bypass） | — |
| 11_subagent | SubAgent 委托 + **Agent Selection Guide** | Subagent（恒 true） | `{{available_agents}}`（SkillsPort 扫描 `.claude/agents/`） |
| 13_skills | Skills 机制 + **通用加载协议**（loading/discovery/catalog/using/suggesting） | Skills（恒 true） | — |
| 15_channel | 频道消息 | Channel（恒 false） | — |
| 16_workflow | Workflow 摘要 + **invoke ultracode skill 指引** | Workflow（executor 存在） | — |

**渲染生成段（波 4 前，非文件）**：`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__`
（:397）、Persona 段（full → agent body；extend → `build_agent_overrides_block`，
:399-407）、Language 段（:424-432）。

**middleware 尾部贡献**（`collect_prompt_contributions`，`stage_builder.rs:410`，
拼在 system prompt 末尾）：AgentsMdMiddleware（CLAUDE.md）、SkillsMiddleware
（skills 摘要）、ToolSearch（deferred 列表 + 工具声明段）、GitAttribution
等——**首轮一次性通知通道**（`trait.rs:70-75`），非段落渲染通道，维持现状。

**消息流注入**（不进 system prompt）：`first_turn_reminder`、system-reminder
消息；`permission_mode_notice_if_changed` **已删除**（演进 1 落地，见 3.5）。

**段落后续归属全景**（目标态 → **已全部落地**）——每段给出动作（保持/合并/
拆分/删除）与归属；gated 段落与现有功能 middleware 主题一一对应：

| 段落 | 现状内容 | 动作 | 归属（目标态） | 重写要点 | 落地 |
| --- | --- | --- | --- | --- | --- |
| 01_intro | 角色定义 + 防御性安全 + URL 纪律（4 行） | 保持 | DefaultSystemPromptMiddleware | 保持独立 ID——meta_harness 场景 1"覆盖角色定义"的核心用例；内容不动 | ✅ |
| 02_system | 惯例遵循 + 安全实践 + Proactiveness | 拆分 | DefaultSystemPromptMiddleware | `# Proactiveness` 块并入 03_doing_tasks（主题：执行模式）；剩余惯例 + 安全实践保持 | ✅ |
| 03_doing_tasks | 任务执行步骤（思考→执行→验证→提问，46 行） | 合并（接收） | DefaultSystemPromptMiddleware | 并入 02 的 Proactiveness 块 | ✅ |
| 04_actions | 操作可逆性 + 简洁性 + Git 安全 | 保持 | DefaultSystemPromptMiddleware | 内容不动 | ✅ |
| 05_using_tools | 工具纪律（批处理/Bash 纪律，不绑定工具名） | 保持 | DefaultSystemPromptMiddleware | meta_harness 场景 1 覆盖用例；内容不动 | ✅ |
| 06_tone_style | 语气与简洁性 | 保持 | DefaultSystemPromptMiddleware | 内容不动 | ✅ |
| 07_env | 环境快照（动态占位符） | **合并** → `07_runtime` | DefaultSystemPromptMiddleware | 与 14 合并为"运行时状态与事件语义"；占位符替换（render 统一替换）不变 | ✅ |
| 14_system_reminder | system-reminder 语义 + 信任边界 | **合并** → `07_runtime`（文件删除） | 同上 | 信任边界（防伪造）内容保留——安全相关 | ✅ |
| 10_hitl | HITL 审批机制 + sensitive 工具列表 + 模式决策 | 拆分（列表） | HumanInTheLoopMiddleware | sensitive 列表 → middleware 按代码事实生成（`default_requires_approval`，assembly.rs:32）；段落只留机制说明（PermissionMode 决策 + 审批决策语义） | ✅ |
| 11_subagent | 委托机制 + catalog + 授权边界 + Agent Selection Guide | 拆分（指南） | SubAgentMiddleware | Agent Selection Guide **删除具体映射**（perihelion 特有调度建议是仓库级知识，catalog id/description 已承载语义）；通用选择原则（specialized 优先 / general-purpose 兜底 / 按 access 并行化）浓缩保留在段落，不绑定 agent 名；段落只留委托机制（方式/授权边界/when to use/writing/fork），落地步骤见 3.5.1 | ✅ |
| 13_skills | Skills 机制 + 通用加载协议（loading/discovery/catalog/using/suggesting） | 拆分（协议细节） | SkillsMiddleware | 协议实现细节（discovery roots 优先级、扫描深度）→ middleware 按实际装配动态生成；机制说明（工具用法/catalog 语义）保留 | ✅ |
| 15_channel | 频道消息格式（gate 恒 false） | 保持 | 未来 channel middleware | 内容不动（gate 恒 false 直至 channel 能力装配） | ✅（保持，无持有者） |
| 16_workflow | workflow 声明 + invoke ultracode 指引（9 行） | **删除** | — | 整段删除（ultracode skill 完整覆盖，见 3.1.2） | ✅ |

**渲染生成段归属**（非段落文件；演进去向见 3.5.2，已落地）：

| 段 | 驱动 | 动作 | 归属（目标态） |
| --- | --- | --- | --- |
| `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` | 渲染逻辑生成 | **删除** | 缓存区划分由段落位置属性（契约 2）显式承担，不再生成文本标记 |
| Persona 段（full/extend） | AgentOverrides（用户配置） | **并入** | DefaultSystemPromptMiddleware（角色/身份持有者）——关闭它 Persona 一并消失（纯净模式清空 persona 覆盖） |
| Language 段 | `settings.language`（用户配置） | **新开 middleware** | 新 `LangMiddleware` 持有（段落 ID `language`）——可关闭、可覆盖 |

**middleware 尾部贡献**（`prompt_contribution`，首轮一次性通知，拼在
system prompt 末尾）：已是 middleware 持有提示词的通道（AgentsMd /
Skills / ToolSearch / GitAttribution），维持现状。

**拆分持有契约**（演进 2 落地的稳定性要求，4 条）：

1. **配置驱动段有明确持有者**：Persona 并入 DefaultSystemPromptMiddleware、
   Language 归 LangMiddleware——两者随持有者关闭而消失（纯净模式下用户
   显式配置被清空，属既定语义）；持有者未提供内容时跳过渲染（契约 4）。
2. **段落位置属性显式化**：每段带位置标记（boundary 前缓存区 / boundary
   后）+ 段内序号；装配期按"位置 + 序号"排序，**不依赖 middleware 链序**
   （blueprint 会变，链序不可作顺序契约）。
3. **段落 + gate 原子迁移**：段落移交给 middleware 时，同批把 gate 判定
   从 `PromptFeatures::detect` 硬编码简化为"持有 middleware 是否在链上"；
   禁止中间态（段落已移走、gate 仍硬编码 true 的双轨）。
4. **运行时缺失防御**：段落从编译期常量变为运行时收集后，middleware 在链
   上但未提供段落 = 跳过渲染不 fail（与 2.3 覆盖文档缺失 warn + 忽略语义
   一致）；DefaultSystemPromptMiddleware / LangMiddleware 默认总是装配，
   除非显式关闭。

**深层含义**：拆分后 gated 段落的 gate 判定可简化为"持有该段的 middleware
是否在链上"——3.4 的"gate 不随装配变化"事实（`PromptFeatures::detect`
硬编码）将被改变，**middleware 关闭自动移除其段落**，段落关闭盲区消失；
2.4 段落覆盖语义不变（覆盖 = 替换持有者对应段落贡献）。

### 3.1.2 重复段识别与处理去向

| 段落 | 重复对象 | 证据 | 处理方向 | 落地 |
| --- | --- | --- | --- | --- |
| 16_workflow | `ultracode` builtin skill | 段落全文仅 9 行（工具声明 + when/how + "invoke the `ultracode` skill"指引，`sections/16_workflow.md`）；ultracode SKILL.md（`skills/builtin/skills/ultracode/`）为完整编排指导 | **删除干净**：段落整体删除，不留摘要——ultracode skill 完整承载 WorkflowTool 的 when/how 指引 | ✅（C2：文件 + 数组项 + 映射表项删除） |
| 13_skills | SKILL.md 通用协议（loading protocol / using skills / suggesting skills） | 13_skills 段落含约 60 行通用协议说明，与单个 skill 内容无关 | **随 SkillsMiddleware 管理**：段落内容移交 SkillsMiddleware 持有（3.1.1 归属全景），middleware 关闭即移除段落 | ✅（C3） |
| 11_subagent | 项目特有 Agent Selection Guide（coder/explorer/plan/code-reviewer/web-researcher/general-purpose 调度指南） | 段落内嵌 perihelion 仓库 agent 调度建议，与通用委托机制混写；`{{available_agents}}` 已提供 catalog | **归 SubAgentMiddleware 管理**：段落（含 `{{available_agents}}` 动态生成）移交 SubAgentMiddleware 持有，middleware 关闭即移除段落 | ✅（C3；Selection Guide 已浓缩为通用原则，`test_subagent_selection_guide_has_no_specific_mapping` 锁定） |
| 10_hitl | sensitive 工具列表 vs 代码事实（`default_requires_approval`，`assembly.rs:32` AutoClassifier） | 敏感列表硬编码在段落（sections/10_hitl.md），修改代码需同步段落，失同步风险 | **随 HumanInTheLoopMiddleware 管理**：sensitive 列表改由 middleware 按代码事实生成，段落只留机制说明（3.1.1 归属全景） | ✅（C3；段落改为运行时判定引导句） |

处理原则：重复内容**删除干净**（16_workflow → ultracode 覆盖）或**随对应
功能 middleware 持有管理**（13_skills → SkillsMiddleware、11_subagent →
SubAgentMiddleware）；不做"段落留摘要 + builtin skill 收纳"方案。

### 3.2 middleware 装配现状

- 蓝本：`peri-agent/src/session/factory.rs:83-114` `production_blueprint()`；
  装配：`peri-middlewares/src/assembly.rs:75-483` `ProductionChainAssembler`
  （Mcp/Workflow/Lsp/Goal/Hook 已是条件注册——"按条件裁剪链"的先例）。
- 工具注册：`peri-agent/src/session/exec/stage_builder.rs:753-762` 每 turn
  `chain.collect_tools(&cwd)` → `tools.insert(name, Arc<dyn BaseTool>)` 进
  shared_tools（注释：同名工具已存在不覆盖）。
- middleware `name()` 清单（装配面，以代码为准）：

| name() | 提供的工具 |
| --- | --- |
| DefaultSystemPromptMiddleware | 无工具（段落持有者：01-06 / 07_runtime / persona；关闭即清除全部基础段与 persona 覆盖） |
| LangMiddleware | 无工具（段落持有者：language；可关闭、可覆盖） |
| FilesystemMiddleware | 6 个文件系统工具 |
| TerminalMiddleware | Bash |
| WebMiddleware | WebFetch / WebSearch |
| ImageMiddleware | 无工具（纯钩子：`@image` → ContentBlock::Image 转换，`image/mod.rs:66-70`；**不是工具提供者**） |
| TodoMiddleware | TodoWrite |
| McpMiddleware | MCP bridges + DiscoverMCPTool |
| CronMiddleware | Cron ×3 |
| SubAgentMiddleware | SubAgentTool + AgentResultTool + 段落 11_subagent |
| SkillsMiddleware | SkillTool + DiscoverSkillsTool + 段落 13_skills |
| WorkflowMiddleware | WorkflowTool（链上实际注册的是 `WorkflowMiddlewareAdaptor`，`workflow/mod.rs:381-383`，name 同名 "WorkflowMiddleware"） |
| ToolSearch | SearchExtraTools / ExecuteExtraTool / ArtifactTool |
| LspMiddleware | LspTool |
| GoalMiddleware | GoalTool |
| HookMiddleware / AgentsMdMiddleware / AtMentionMiddleware / AgentDefineMiddleware / GitAttributionMiddleware / HumanInTheLoopMiddleware（+ 段落 10_hitl）/ SkillPreloadMiddleware / PluginMiddleware | 无工具（提示词/钩子贡献） |

**波 4 后**：`MIDDLEWARE_NAMES` 编译期清单 = 上表全部 23 项
（`peri-acp-types/src/meta_harness.rs`，新增 DefaultSystemPromptMiddleware /
LangMiddleware 两项；顶层链 / Workflow agent 链 / 子链并集，与
blueprint/name 映射由 `assembly_test.rs` 锁定）。

### 3.3 settings 解析与合并现状

- `PeriConfig { schema, config: AppConfig }`（`peri-acp/src/provider/config.rs:11-17`）；
  `AppConfig`（`:140-184`）为用户配置面（`persona`/`tone`/`language` 等，多数
  Option 字段 `#[serde(default, skip_serializing_if = "Option::is_none")]`；
  **例外**：`skills_dir`（`:151`）只有 `default + alias`，无 skip）。
- 全局 `~/.peri/settings.json` 经 `provider/store.rs:70-77`（`load`）与
  `:80-87`（`load_from`）解析；项目级 `{cwd}/.peri/settings.json` 经
  `merge_overrides`（`provider/config.rs:189-240`）合并——**现状是整体覆盖**
  （Option Some 即整体替换，`config.rs:210-237`），`env` 字段行为有测试锁定
  （`config_test.rs:83-97`，项目级 FOO 会抹掉全局 BAR）；逐 key 合并无先例
  （理想 2.1 要求新增专属特例）。
- **merge 接线事实（实现裁定 Q5，advisor 复审后修正）**：`merge_overrides`
  特例分支**已接生产线路**——`store.rs:70-77` 的 `load()`（全局 + 工作区
  双文件，`:74` 调用 `merge_overrides`）由宿主经
  `peri-tui/src/config/mod.rs` re-export 后在 `peri-tui/src/app/mod.rs:53`
  调用；项目级 `.peri/settings.json` 的 meta_harness 与其余字段一并生效。
  早期"无接线"记录有误，已由契约测试锁定
  （`store_test::test_load_merges_meta_harness_per_key`）。
- `SessionManager` 持有 `peri_config`（`session/mod.rs:129`，accessor `:362`），
  冻结期可直达（`:467`）；**装配期不可直达**——`AssemblyContext`
  （`factory.rs:229`）无 `peri_config` 字段（理想 2.5 要求新增字段透传）。

### 3.4 现状与理想架构的差距（落地定案映射）

**PromptTemplate 构造点全量清单**（理想 2.4"同源一致性"的现状落点；实现
裁定 Q6——以当前代码实际构造表达式为准，旧行号已漂移）：

| 调用点 | 场景 |
| --- | --- |
| `peri-acp/src/session/mod.rs:481` | build_frozen_data 冻结渲染（**一次构造一次渲染**；`subagent_system_prompt` 二次渲染已随 C5 移除，子面向复用主冻结 prompt） |
| `peri-acp/src/host/stage_builder.rs` | agent_overrides 重渲染 / SubAgent 回退渲染（ACP host 投影层） |
| `peri-acp/src/host/workflow_agent.rs` | workflow agent 渲染（fallback + agentType builder；按 advisor 裁决 B 过滤 `10_hitl`——workflow 链不装配 HumanInTheLoopMiddleware） |
| `prompt/mod.rs` `build_system_prompt` helper | **仅测试直接调用**，非生产路径 |

波 4 后全部构造点经 `build_collected_sections(state, overrides, language)`
计算收集结果（渲染面静态声明，与链侧 `collect_prompt_sections` 同一段声明
函数——单一事实源禁止双轨；冻结 disabled 集合驱动，链未装配的构造点也能
得到与装配一致的段落）。

**冻结载体连锁**（理想 2.3"挂载要求"的现状落点，breaking 改动；实现裁定
Q3/Q7）：
`FrozenContext`（`store.rs:56`，5 字段）**单份新增 `meta_harness` 字段**，
`FrozenSessionData`（`executor.rs:141`，2 字段，经委托字段 `v2_frozen`
存储；`subagent_system_prompt` 已随 C5 移除）提供 accessor，
`from_frozen_parts`（`executor.rs:157-163`）不加重复
参数；**全部构造点须同步**：`session/mod.rs:518`、`host/prompt.rs:418`、
空冻结回退（`executor.rs:1169-1178`）、子 session 复制（`subagent.rs:495-507`
与 `:894-906`）、`FrozenContextBuilder::build`（`store.rs:118-125`）与各
测试直接构造点；字段采用 `Arc<str>` 约定（与 FrozenContext 既有字段一致）。

**装配入口全量清单**（理想 2.5"关闭面 = 全部装配入口"的现状落点，5 处；
仅过滤顶层链会产生链下泄漏；第 5 处为实现裁定 Q9）：

| 装配入口 | 位置 | 内容 |
| --- | --- | --- |
| 顶层链 | `assembly.rs:259-451`（23 注册点，波 4 后新增 DefaultSystemPromptMiddleware / LangMiddleware） | 主链全部 middleware |
| parent_tools | `assembly.rs:186-197` | **无条件** Filesystem+Bash+Web+MCP 工具 → SubAgentMiddleware（`subagent/mod.rs:158-168` → `:244-246`） |
| Workflow agent 链 | `assembly.rs:670-715` | 独立装配 FileSystem/Web/Terminal/Todo/GitAttribution |
| 子链 | `subagent/tool/mod.rs:56-64`（装配入口 `SubagentChainAssemblerImpl`，`:125-139`） | AgentsMd → Skills → SkillPreload（条件）→ **Todo（无条件）** |
| /bg 后台 agent | `host/prompt.rs:385-405`（`parent_tools_factory`） | **无条件** Filesystem+Terminal+Web 工具（MCP 有意排除）——已按 `disabled_middlewares` 过滤 |

**AskUserQuestion 架构**（理想 2.5"不在关闭面"的依据）：

- **定义**：`peri-middlewares/src/tools/ask_user_tool.rs:21`
  `AskUserTool { broker: Arc<dyn UserInteractionBroker> }`——唯一依赖是人机
  交互 broker，无 middleware 链参与。
- **契约**：`UserInteractionBroker`（`peri-acp-types/src/interaction.rs:113`）
  统一 HITL（工具审批）与 AskUser（问答）两条路径：`request(InteractionContext)`
  挂起等待用户响应，由应用层（TUI/CLI/测试）实现。
- **注册**：`assembly.rs:183` 构造——**刻意使用原始 broker 而非
  MultiplexBroker**（ChannelBroker 对 Questions 立即返回空答案、Multiplex
  竞速时 Channel 先返回，会绕过弹窗）；`assembly.rs:460-463` 直接
  insert shared_tools（v2 stages 不走 execute() 的 register_tool 合并；
  collect_tools merge 时同名不覆盖）。
- **行为**：`is_direct() = true`（Core 层，始终对 LLM 可见）、
  `namespace = "interaction"`、`timeout() = None`（无限期等待用户）、
  `prompt_declaration` 含批量合并纪律；调用时 1-4 个问题解析为
  `QuestionItem` → `broker.request` 挂起。
- **与 HITL 的关系**：审批（`ApprovalItem`/`HumanInTheLoopMiddleware`）与
  提问（`QuestionItem`/`AskUserTool`）共用 `InteractionContext`/`InteractionResponse`
  契约；`SpeculationGuard`（`peri-agent/src/agent/stages/mod.rs:550` 注释、
  字段 `asked_user` 在 `:551`）按"本轮是否已调用 AskUserQuestion" 做决策
  保护。
- **为何不可关闭**：非 middleware 提供（无 collect_tools 路径），meta_harness
  的 `false` 面无法触及；且对话必须保留向用户提问的通道（HITL 审批拒绝、
  歧义澄清都依赖它）。若要纳入关闭面，需将其移入某个 middleware 的
  collect_tools 或显式建模为"可关闭能力"——本期不做，记为未来项。

**gate 判定事实（波 4 C3 后已改变）**：波 4 前 `PromptFeatures::detect`
（`prompt/mod.rs:42-52`）的 gate 判定**不随 middleware 装配变化**——
`subagent_enabled` / `skills_enabled` 恒 `true`，`hitl_enabled` 由权限模式
决定，`workflow_enabled` 由 workflow executor 参数决定，`channel_enabled`
恒 `false`；关闭 SubAgentMiddleware 后 `11_subagent` 段落仍渲染内置内容
（段落是 middleware 关闭的盲区）。**C3 落地后（契约 3，gate 原子迁移）**：
`FeatureGate` 仅剩 `Channel` 一项（15_channel，无持有者，恒 false）；
gated 段（10_hitl / 11_subagent / 13_skills）gate = 持有 middleware 是否在
链上（收集即装配），**middleware 关闭自动移除其段落，段落关闭盲区闭合**；
段落覆盖（2.4）语义不变（覆盖 = 替换持有者对应段落贡献）。

### 3.5 演进方向

**演进 1：permission_mode_notice 删除（已落地，C4）。**
`permission_mode_notice_if_changed`（原
`peri-agent/src/session/exec/executor.rs:497-519`）是 Executor 直接注入消息
流的运行时通知，非 middleware 产出，不在 meta_harness 关闭面内——已整体
删除该注入（无需 notify），此项不再是"纯净"死角（代码全链已无
permission_mode_notice / ModeNoticeBooking / mark_permission_mode_notified
引用）。

> **影响面（已同步清理）**：注入文本、session 级 `last_notified` 状态、
> `ModeNoticeBooking`（`executor_helpers.rs`）、`mark_permission_mode_notified`
> Phase 6 记账、哨兵与相关测试。
> **语义代价（已落定）**：删除后模型失去"权限模式"感知通道——10_hitl
> 段落现明确告知 "There is no runtime notification when it changes: the
> approval decision you observe on each tool call reflects the current mode
> at evaluation time"（段落文本承载缺失告知，C4 遗留 3）；Bypass 模式下
> 10_hitl 随持有者装配恒渲染（内容为条件式事实描述，与 Bypass 无矛盾，
> C3 D5 裁定）。删除未改变模型在 Default/AcceptEdit/AutoMode 下对审批边界
> 的认知（决策以每个工具调用的实时评估为准）。

**演进 2：系统提示词段拆分持有（已全部落地，wave3-4 workflow C1-C5）。**
`prompt/mod.rs` 的内置段落从单一持有者拆分给多个 middleware 持有，段落
组织已完成重写（删除/合并/拆分明细与归属全景见 3.1.1，逐项落地标注）：

- ✅ **gated 段落随对应功能 middleware 走**：10_hitl → HumanInTheLoopMiddleware
  （sensitive 列表改为代码事实生成）、11_subagent → SubAgentMiddleware
  （Agent Selection Guide 删除具体映射、通用原则浓缩，落地步骤见 3.5.1）、
  13_skills → SkillsMiddleware（协议细节按实际装配生成）、16_workflow →
  **整段删除**（ultracode skill 覆盖）、15_channel 待未来 channel
  middleware——
  **gate 判定简化为"持有 middleware 是否在链上"**（契约 3，收集即装配），
  3.4"gate 不随装配变化"事实已改变，middleware 关闭自动移除其段落，
  段落关闭盲区消失；
- ✅ **01-06 / 07_runtime / Persona 归 DefaultSystemPromptMiddleware**（基础
  能力持有者：01 身份与安全、02 惯例、03 任务执行、04 操作、05 工具纪律、
  06 语气、07_runtime = 07_env + 14_system_reminder 合并的环境快照与事件
  语义、Persona = overrides 动态段）：关闭它即清除全部内置段落内容与
  persona 覆盖；✅ **Language 归新开 LangMiddleware**（可关闭、可覆盖，见
  3.5.2）；✅ **boundary 标记删除**（缓存区划分由位置属性承担，见 3.5.2）；
- ✅ 段落覆盖（2.4）语义保留：覆盖 = 替换持有 middleware 的对应段落贡献；
- ✅ 重复段收敛（3.1.2）同步进行：16_workflow 整段删除（ultracode 完整覆盖）；
  13_skills 协议、11_subagent 的 Agent Selection Guide 随对应功能
  middleware 持有管理；
- ✅ **拆分持有契约**（3.1.1）逐条落地：段落 + gate 原子迁移（契约 3）、
  段落位置属性显式化（契约 2）、运行时缺失防御（契约 4）；
- ✅ 演进后"完全纯净" = 关闭 DefaultSystemPromptMiddleware + LangMiddleware +
  关闭其余 middleware（AskUserQuestion 除外，见 3.4），与运行时注入通道
  （演进 1 删除后）齐平。

**对账裁决（2026-08-14，wave3-4 落地后逐项对账：设计授权 vs 实现偏差）**：

| 设计条目 | 裁决 | 依据 |
| --- | --- | --- |
| 02 Proactiveness 并入 03 | 设计授权 | 3.1.1 归属全景原文"`# Proactiveness` 块并入 03_doing_tasks（主题：执行模式）"；03 尾部含 Proactiveness 块、02 无 |
| 07_env + 14_system_reminder 合并为 07_runtime | 设计授权 | 3.1.1 原文"与 14 合并为运行时状态与事件语义"；07_runtime 含 env 快照 + System Reminders（信任边界保留，安全相关） |
| 16_workflow 整段删除 | 设计授权 | 3.1.1/3.1.2 原文"整段删除（ultracode skill 完整覆盖）"；文件 + GATED_SECTIONS 项 + SECTION_HOLDER_MIDDLEWARE 项已删 |
| gated 段 gate 简化为"持有者是否在链上" | 设计授权 | 3.5 原文；SECTION_HOLDER_MIDDLEWARE + project_enabled_sections 落地 |
| 10_hitl sensitive 列表改运行时引导句 | 设计授权 | 3.1.1 原文"列表 → middleware 按代码事实生成"；段落改 `default_requires_approval` 引导句 |
| Persona 三态"无 overrides → 不渲染" | **实现偏差（行为等价 + 改进）** | C2 D2：落地为"声明空段 + `PromptTemplate::new` 空过滤"——保证 `.peri/meta/persona.md` 覆盖在无 overrides 时仍可注入（覆盖先于空过滤生效）；无覆盖时空段不渲染，行为与设计等价 |
| 子面向二次渲染（subagent_system_prompt） | **实现偏差（C5 收口）** | C2 D5 曾计划"字段保留为遗留"；C5 实际移除字段，子面向复用主冻结 prompt（两版字节相同，16_workflow 删除后无差异） |
| permission_mode_notice 删除的 10_hitl 同步 | 设计授权（演进 1 原文） | 段落现含 "There is no runtime notification when it changes" 缺失告知 |

> **语义边界**：演进 2 只收敛"段落内容来源"（include_str! 常量 → middleware
> 持有），**不得改变**：① 段落顺序与渲染机制——`prompt_contribution`
> （`trait.rs:70-75`）是**首轮一次性通知**，非段落渲染通道，middleware 持有
> 的段落仍走 `PromptTemplate` 段落装配渲染（按位置属性 + 段内序号 + gate
> 决定，见 3.1.1 拆分持有契约 2），middleware 仅作为内容载体；② 未拆分段落的
> gate 判定——拆分落地后 `PromptFeatures::detect`（`prompt/mod.rs`）仅剩
> `Channel` 硬编码判定（15_channel 无持有者，恒 false），其余段落 gate 均由
> 收集机制承担（契约 3）。

### 3.5.1 落地样例：11_subagent 拆分步骤（试点，已全部落地）

**波 4 前结构**（`sections/11_subagent.md`，75 行，8 个子节；归档）：

| 子节 | 处理 |
| --- | --- |
| SubAgent Delegation（工具声明） | 保留 |
| Available agent types（`{{available_agents}}` 占位符 + tier/access 语义） | 保留（占位符替换机制不变，见步骤 2） |
| Authorization boundary（授权边界） | 保留（10_hitl 的 "(see 11_subagent)" 交叉引用依赖它） |
| When to use sub-agents | 保留 |
| **Agent Selection Guide**（:24-40：具体任务→agent 映射 + Standard pipelines + Parallelization 建议） | **删除具体映射**：perihelion 特有调度建议（coder/explorer/plan…）是仓库级知识，catalog id/description 已承载语义；**通用选择原则**（specialized 优先 / general-purpose 兜底 / 按 access 标签并行化）浓缩为 2-3 句保留 |
| Writing the prompt / Fork mode / Usage notes / Background Tasks | 保留 |

**落地步骤**（步骤 1 可独立合入；步骤 3+4 必须同批，契约 3；**已全部执行**）：

1. ✅ **段落内容重构**（纯内容变化，行为不变）：删除 Selection Guide 具体映射与
   pipelines，浓缩通用选择原则（不绑定 agent 名）；11_subagent.md 约 75 → 50
   行；同步更新 prompt 渲染测试的逐字守护快照。
2. ✅ **`{{available_agents}}` 占位符机制不变**：catalog 替换留在渲染层
   （`format_available_agents`，prompt/mod.rs），SubAgentMiddleware 持有的
   是含占位符的段落文本（middleware 仅作内容载体，语义边界 ①）。
   **同源一致性检查**：render 的 catalog（`SkillsPort::agents`）与
   SubAgentMiddleware 的 `scan_agents`（subagent/mod.rs:278，含
   `list_built_in_agents` 兜底）扫描同一目录（`.claude/agents/` + built-in），
   是同一 agent 集合的两个视图——落地时确认两处实现一致（或收敛为共享
   实现），防止提示词 catalog 与子链实际可用 agent 不一致。
3. ✅ **段落持有迁移**（机制变化）：前置为段落持有者接口（契约 2 的位置属性 +
   装配期收集）；11_subagent 内容从 `GATED_SECTIONS` 常量数组 → 由
   SubAgentMiddleware 持有（文件可留在 `sections/` 由 middleware
   `include_str!`，内容不复制）。
4. ✅ **gate 原子迁移**（行为变化）：`FeatureGate::Subagent` 从
   `PromptFeatures::detect` 硬编码（恒 true）→ 装配期判定"SubAgentMiddleware
   是否在主链"，冻结进 capability snapshot；与步骤 3 同批（禁止"段落已移走、
   gate 仍 true"双轨）。**子链语义保持**：子 agent 渲染继承主链 snapshot
   （除 workflow），11_subagent 在子 agent 提示词中的存在性不变（现状行为）；
   "子链无 SubAgentMiddleware → 子 agent 提示词失去委托段"与"子 agent 不继承
   Agent 工具"更一致，但属行为变化，列为待评估项，本期不做。关闭
   SubAgentMiddleware → 11_subagent 段落 + SubAgentTool/AgentResultTool
   同时消失，盲区闭合。
5. ✅ **覆盖语义兼容**（2.4）：meta_harness `"11_subagent": true` →
   `.peri/meta/11_subagent.md` 在 `PromptTemplate::new` 构造期按 ID 替换
   持有者段落，机制与持有者无关，无需特殊处理。
6. ✅ **测试计划**：渲染测试（重构后输出 + 子 agent 继承性）；catalog 同源
   一致性测试（`format_available_agents` vs `scan_agents`）；assembly 测试
   （关闭 SubAgentMiddleware → 段落与工具同时消失）；覆盖测试
   （`11_subagent` 覆盖输出 = md 全文，段落顺序不变）。

### 3.5.2 落地样例：渲染生成段重构（boundary 删除 / Persona 并入 / LangMiddleware，已全部落地）

三个渲染生成段（非段落文件）的演进去向与落地步骤（**已全部执行**）：

**步骤 1：boundary 删除（`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__`，✅ 已落地）**

- render 删除标记拼接（prompt/mod.rs:397 附近）；影响：缓存前缀范围变化——
  现状 boundary 前 = 01-06（缓存命中区）、boundary 后 = 07/14/gated/Language
  （非缓存区）；删除后全部段落连成一体，缓存前缀边界由 provider 前缀匹配
  决定。段落仍会话级冻结（ARC-FROZEN-001），provider 侧 `cache.rs` /
  `request.rs` 保留向后兼容兜底（找不到标记 → 整个 prompt 作为单个缓存块，
  Review 确认为良性全缓存路径）。
- 段落位置属性（契约 2）保留：boundary 前/后作为段落"缓存区"位置标记继续
  存在（装配机制），只是不再生成提示词文本标记。
- 测试：渲染输出无 boundary 标记；渲染结构（段落顺序）不变。

**步骤 2：Persona 并入 DefaultSystemPromptMiddleware（✅ 已落地）**

- Persona 成为 DefaultSystemPromptMiddleware 的动态段（id `persona`）：
  full → agent body；extend → `build_agent_overrides_block`；无 overrides →
  **声明空段 + `PromptTemplate::new` 空过滤**（C2 D2 实现偏差，行为与
  "不渲染"等价；空段声明保证 `.peri/meta/persona.md` 覆盖可注入）。
- 渲染位置保持：非缓存区首位（07_runtime 之前，位置属性 = Uncached + 段内
  序号 0）。
- 关闭 DefaultSystemPromptMiddleware → Persona 段一并消失（纯净模式清空
  persona 覆盖）；meta_harness 可覆盖 `persona` 段
  （`.peri/meta/persona.md`，场景 1 语义）。
- 测试：full/extend/无 overrides 三态渲染；关闭持有者 → persona 消失；
  覆盖 `persona` 段输出 = md 全文。

**步骤 3：Language 新开 LangMiddleware（✅ 已落地）**

- 新 middleware `LangMiddleware`（peri-middlewares，name = "LangMiddleware"），
  持有 Language 段（id `language`，内容 = `settings.language` 映射的指令
  文本，`map_language_to_instruction` 逻辑随持有者迁移）。
- 位置属性 = 非缓存区最后段（gated 之后，段内序号 7）。
- 可关闭：meta_harness `"LangMiddleware": false` → Language 段消失（模型
  失去语言指令，属用户选择）。
- 可覆盖：meta_harness `"language": true` → `.peri/meta/language.md` 全文
  替换语言指令。
- render 的 `language` 参数与全部构造点（3.4 清单）同步调整：语言指令不再
  由 render 参数拼接，改由 LangMiddleware 持有内容（含覆盖合并）。
- 测试：装配默认输出与现状一致；关闭 → 段落消失；覆盖 `language` 段；
  构造点同源一致性（防双轨）。

---

## 4. 附：调研存档

落地性深度调查（3 条链路并行验证，70+ 项引用核对）原始报告：
`.peri/plans/meta-harness-config-chain-validation.md`、
`.peri/plans/meta-harness-landability.md`、
`.peri/plans/meta-harness-verification.md`。
