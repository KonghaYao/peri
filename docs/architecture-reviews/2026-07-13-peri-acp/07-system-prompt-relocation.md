# 候选 7：把 build_system_prompt 从 peri-acp 移到独立 prompt crate

> 日期：2026-07-13 | 模块：`peri-acp/src/prompt/`（321 LOC）+ `prompts/sections/`（14 段落）| 类型：架构走读
> 流程：/grilling（Speculative seam 错层深化）
> 范围：`prompt/mod.rs:116-252` 的 `build_system_prompt()` + `PromptFeatures` / `PromptEnv` + 14 个 `.md` 段落 + `session/executor.rs::FrozenSessionData::build` 中的调用点

---

## 1. 摘要

`peri-acp/src/prompt/mod.rs:116-252` 的 `build_system_prompt()` 是 peri-acp 内最深的**纯逻辑模块**（321 LOC 实现 + 491 LOC 测试，27 条单测，0 外部副作用），它定义的是 **agent 行为**——按设计文档 `docs/design/peri-agent-acp-v2.md` 原则 5（"Provider 配置独立——ACP 构建好后注入 agent"）和 `docs/design/peri-agent-system-prompt-v2.md` 全文，它本质上属于 peri-agent 范畴。这是一个 **Speculative** 候选：当前位置并未引发 bug，迁移的目的是"对齐模块边界与语义责任"，而非修复具体缺陷。

本候选走 /grilling 流程，审慎评估三个方向（A 独立 `peri-prompt` crate / B 迁入 `peri-agent` / C 保持现状）。结论倾向于 **C（保持现状 + 文档化 seam）** 作为本周期推荐，但给出 A/B 的完整草案与触发条件——任何迁移都必须先破坏 prompt cache 不变量（SP 结构任何变化都让 Anthropic Prompt Cache 失效），这是一个高代价、低收益的物理操作。文档末尾附 ADR 草案明确记录决策。**这是 Speculative 候选，落地前必须走 ADR 评审，否则不得动 prompt 模块**。

---

## 2. 现状诊断

### 2.1 build_system_prompt 的复杂度证据

`prompt/mod.rs` 全文 321 行（含 doc comment 和测试模块声明），核心函数 `build_system_prompt` 占用 116-252 行（136 LOC 函数体）。函数职责清单：

| # | 职责 | LOC 范围 | 性质 |
|---|------|---------|------|
| 1 | 构造 `PromptEnv`（含 frozen_date 分支） | 124-128 | 纯 |
| 2 | 装配 7 个**静态段落**（`01_intro`..`16_workflow`）通过 `include_str!` | 130-161 | 编译期 |
| 3 | 装配**动态段落**（`07_env` + `14_system_reminder`） | 163-172 | 条件 |
| 4 | 5 个 **feature-gated 条件段**（hitl/subagent/cron/skills/channel） | 173-202 | 条件 |
| 5 | 构造 `AgentOverrides` 覆盖块 | 204-206 | 纯 |
| 6 | 拼接静态段 + `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` + overrides + 动态段 | 213-228 | 纯 |
| 7 | 注入 language 指令（动态，边界之后） | 230-237 | 条件 |
| 8 | 6 个环境占位符替换（`{{cwd}}`/`{{is_git_repo}}`/`{{platform}}`/`{{os_version}}`/`{{date}}`/`{{available_agents}}`） | 240-251 | 纯 |

```rust
// prompt/mod.rs:116-252 的签名（精简）
pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String
```

**关键观察**：

1. **零外部 I/O**：除 `PromptEnv::detect()` 读 `.git` 目录、`format_available_agents()` 调用 `scan_agents_detailed()` 之外，函数体是纯字符串拼接。
2. **零 trait 依赖**：仅吃 `&str` / `Option` / `&[PathBuf]` / `AgentOverrides`（来自 `peri-middlewares`）。
3. **零 LLM 依赖**：不 import `peri-agent` 任何类型。**这是它仍在 peri-acp 的最强动机**——它**不**与 ReAct 引擎耦合。
4. **测试覆盖良好**：`prompt_test.rs` 491 LOC，27 条 `#[test]`，覆盖了静态段顺序、feature-gated 段、boundary marker 位置、overrides 注入位置、placeholder 替换、language 注入、空目录处理等。

### 2.2 frozen 数据流（CLAUDE.md / 设计文档摘录）

`CLAUDE.md` 描述的 frozen data flow：

```
frozen_date → frozen_claude_md → frozen_skill_summary → frozen_system_prompt
```

实现在 `peri-acp/src/session/executor.rs:143-188` 的 `FrozenSessionData::build`：

```rust
// executor.rs:162-170
let features = crate::prompt::PromptFeatures::detect();
let system_prompt = crate::prompt::build_system_prompt(
    None,
    cwd,
    features,
    plugin_agent_dirs,
    Some(frozen_date),
    language,
);
```

构造产物委托给 `peri_agent::session::FrozenContext`（v2 迁移已建立的 seam）：

```rust
// executor.rs:175-181
let v2_frozen = peri_agent::session::FrozenContext {
    system_prompt: Arc::from(system_prompt),
    claude_md: claude_md.clone().map(Arc::from).unwrap_or_default(),
    skill_summary: skill_summary.clone().map(Arc::from).unwrap_or_default(),
    date: Arc::from(frozen_date),
    language: language.map(|l| Arc::from(l.to_string())),
};
```

`peri_agent::session::FrozenContext` 已经存在于 peri-agent，是 SubAgent frozen 数据透传的载体。**注意**：`system_prompt` 字段在 peri-agent 侧是 `Arc<str>`，构造时机仍是 peri-acp 调用 `build_system_prompt` 后注入——这正是原则 5 的具体落地："Provider 配置独立——ACP 构建好后注入 agent"，同样的模式也适用于 prompt。

### 2.3 14 段落 + 6 条件段 + override block + env 替换

`peri-acp/prompts/sections/` 目录共 14 个 `.md` 文件（336 LOC 总文本）：

| 段落 | LOC | 静态/动态 | feature gate | 编号意图 |
|------|-----|----------|-------------|---------|
| 01_intro | 4 | 静态 | — | agent 介绍 |
| 02_system | 19 | 静态 | — | 系统约束 |
| 03_doing_tasks | 34 | 静态 | — | 任务执行 |
| 04_actions | 28 | 静态 | — | 行为规范 |
| 05_using_tools | 27 | 静态 | — | 工具使用 |
| 06_tone_style | 27 | 静态 | — | 风格 |
| 16_workflow | 9 | 静态 | — | workflow（追加） |
| 07_env | 9 | **动态** | — | cwd/git/platform |
| 14_system_reminder | 12 | **动态** | — | reminder |
| 10_hitl | 22 | **动态** | `hitl_enabled` | HITL |
| 11_subagent | 65 | **动态** | `subagent_enabled` | SubAgent |
| 12_cron | 17 | **动态** | `cron_enabled` | Cron |
| 13_skills | 26 | **动态** | `skills_enabled` | Skills |
| 15_channel | 37 | **动态** | `channel_enabled` | Channel |

合计：7 静态 + 7 动态（含 5 feature-gated）= 14 段。`include_str!` 编译期嵌入，路径基于 `env!("CARGO_MANIFEST_DIR")`——**这一点决定了 14 个 .md 文件的物理位置必须与 `include_str!` 的 crate 同源**，迁移代码必须连带迁移段落文件，否则要改成相对路径或嵌入到二进制常量。

### 2.4 prompt cache 不变量

`docs/design/peri-agent-system-prompt-v2.md` §1 五条原则中，对 cache 稳定性有三条强约束：

1. **会话冻结**：System Prompt 在 `session/new` 时构建一次，产出 `frozen_system_prompt`，会话内复用。
2. **静态/动态分离**：`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记将 SP 分为静态区（跨会话不变，参与 Anthropic Prompt Cache）与动态区（会话语境可变，不参与 cache）。
3. **编译期嵌入**：所有段落 `include_str!` 编译期嵌入，运行时不读盘。

实测 `prompt_test.rs` 中有 5 条直接守护 boundary 不变量的测试：

```rust
// prompt_test.rs:216
fn test_boundary_marker_present() { ... }
// prompt_test.rs:225
fn test_boundary_marker_before_dynamic_content() { ... }
// prompt_test.rs:241
fn test_boundary_marker_with_all_features() { ... }
// prompt_test.rs:263
fn test_overrides_after_boundary_marker() { ... }
// prompt_test.rs:455
fn test_language_section_after_boundary_marker() { ... }
```

**任何迁移方案都必须保证这 5 条测试在新 crate 中继续通过，且 boundary 字符串 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 字节级一致**。

---

## 3. 约束

迁移讨论受以下**硬约束**制约（任一破坏即回滚）：

### 3.1 Prompt cache：SP 结构不可变

Anthropic Prompt Cache 基于字节前缀匹配。静态段落的**任何**字节变化（包括顺序、空白、换行符）都会破坏 cache 命中率。这意味着：

- 14 个 .md 文件的内容**字节级**不可变（除非有意识地更新 cache 版本）。
- 静态段落的**拼接顺序**不可变（`01` → `02` → ... → `16`）。
- boundary 标记字符串 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 不可变。
- 迁移本身不改变 SP 内容，但**任何代码改动都可能引入隐性 diff**（例如 `include_str!` 路径解析方式变化、拼接顺序笔误）。

**守护手段**：在迁移前后必须运行 `prompt_test.rs::test_no_overrides_contains_all_sections` + 一个新增的**快照测试**（snapshot），断言 `build_system_prompt()` 输出字节级等于迁移前的 golden 文件。

### 3.2 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 静态/动态分隔

boundary 字符串是 Anthropic prompt cache 命中前缀的物理切分点。设计原则 2 明确：仅静态区域参与 cache。迁移不得：

- 改变 boundary 字符串内容
- 改变 boundary 在输出中的位置（必须在第 7 个静态段之后，overrides / 动态段之前）
- 把当前静态段降级为动态段（如把 `06_tone_style` 移到 boundary 之后会让前缀缩短，cache 命中率下降）

### 3.3 frozen data 透传给 SubAgent

`CLAUDE.md` 陷阱速查明确："SubAgent frozen：必须复用 main agent frozen 数据，禁止重新读盘"。`FrozenSessionData::build` 构造的 `system_prompt` 会通过 `peri_agent::session::FrozenContext` 传给 SubAgent。迁移后：

- peri-acp 调用 `build_system_prompt` 的入口（`executor.rs:163` / `executor.rs:879`）必须继续可用。
- 输出类型 `String` → `Arc<str>` 的转换点（`executor.rs:176`）不变。
- SubAgent 侧（`peri-middlewares/src/subagent/`）不接触 `build_system_prompt`，只消费 `FrozenContext.system_prompt`。

### 3.4 中途纠正消息必须用 `BaseMessage::human()`

`CLAUDE.md` 陷阱速查明确："中途纠正消息用 `BaseMessage::human()`，禁止 `BaseMessage::system()`（invoke.rs 会 hoist 污染 frozen prompt）"。

这与本候选的关系：迁移 `build_system_prompt` **不影响**这条约束——`build_system_prompt` 产出的是顶层 frozen SP，不进入 `AgentState.messages`。设计原则 5："不与 Messages 耦合"。但**任何迁移方案都必须重新核验** invoke.rs 的 system hoist 逻辑，确保迁移后 peri-agent 不会因为 prompt 模块"近水楼台"而把中途 `system()` 消息绕回 `build_system_prompt`。

---

## 4. 依赖关系

### 4.1 前置依赖

| 候选 | 状态 | 关系 |
|------|------|------|
| **候选 3（中间件链迁回 peri-agent）** | 待评审 | **强前置**。如果中间件链能从 peri-acp 迁回 peri-agent，则 `build_system_prompt` 的迁移有先例可循、有路径可走；如果候选 3 否决，本候选大概率也否决——两者面临同样的方向性 seam（peri-acp 是否应该 hold agent 行为定义）。 |

### 4.2 后置依赖

无。本候选是**边界讨论**，不阻塞其他候选。即便否决（保持现状），候选 6（trait 抽取）也能独立推进——把 `build_system_prompt` 包成 trait 让 builder 可测，不需要换 crate。

### 4.3 平行依赖

| 候选 | 状态 | 关系 |
|------|------|------|
| **候选 6（trait 抽取让 build_system_prompt 可测）** | 待评审 | **平行/互补**。候选 6 引入 `SystemPromptBuilder` trait 让 `build_agent` 可注入 fake；本候选讨论 trait 的实现放哪个 crate。若候选 6 落地，本候选的讨论更有抓手（trait 在新 crate / 旧 crate / peri-agent，三选一）。 |

---

## 5. 加深后的模块形状

三个方向的 Rust interface 草案与权衡。

### 5.1 方向 A：独立 `peri-prompt` crate

#### 5.1.1 目录结构

```
peri-prompt/
├── Cargo.toml                    # 无依赖或仅依赖 chrono / peri-middlewares（AgentOverrides）
├── src/
│   ├── lib.rs                    # pub use builder::*;
│   ├── features.rs               # PromptFeatures
│   ├── env.rs                    # PromptEnv
│   ├── overrides.rs              # build_agent_overrides_block（从 peri-acp 迁入）
│   ├── os_version.rs             # os_version_string
│   ├── language.rs               # map_language_to_instruction
│   └── builder.rs                # build_system_prompt + format_available_agents
├── sections/                     # 14 个 .md 段落（从 peri-acp/prompts/sections/ 迁入）
│   ├── 01_intro.md
│   ├── ...
│   └── 16_workflow.md
└── tests/
    └── snapshot.rs               # 新增字节级快照测试
```

#### 5.1.2 Interface 草案

```rust
// peri-prompt/src/lib.rs
pub use builder::build_system_prompt;
pub use env::{PromptEnv, os_version_string};
pub use features::PromptFeatures;
pub use language::map_language_to_instruction;
pub use overrides::build_agent_overrides_block;

// peri-prompt/src/builder.rs
use peri_middlewares::AgentOverrides;

pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String {
    // 与 prompt/mod.rs:116-252 字节级一致
    // include_str! 路径改为相对本 crate：
    let static_sections: &[&str] = &[
        include_str!("../sections/01_intro.md"),
        // ...
    ];
    // ...
}
```

#### 5.1.3 迁移成本

| 项 | 成本 | 备注 |
|----|------|------|
| 新建 crate + Cargo.toml + workspace 注册 | 低 | 1 次提交 |
| 物理移动 14 个 .md 文件 | 低 | `git mv` |
| 物理移动 321 LOC Rust 代码 | 中 | 拆分到子模块，调整 `include_str!` 路径 |
| 移动 491 LOC 测试 | 低 | `git mv`，调整 `#[path]` |
| `peri-acp` 改 import：`use peri_prompt::build_system_prompt` | 低 | 2 处调用点 |
| 新增字节级快照测试守护 cache 不变量 | **中** | 必须**先**在旧位置生成 golden，再迁移，再断言 |
| Workspace `members += ["peri-prompt"]` | 低 | 1 行 |

#### 5.1.4 收益

- **边界清晰**：prompt 装配与 ACP session 编排物理分离。
- **可复用**：未来若有 `peri-cli`（非 TUI 入口）或其他 agent 实现，可直接依赖 `peri-prompt` 而不拉 `peri-acp`（包含 ACP 协议代码）。
- **测试独立**：`peri-prompt` 可独立运行 `cargo test -p peri-prompt`，编译时间短（不依赖 ACP SDK / Langfuse / ratatui）。
- **编译并行**：拆分 crate 增加 cargo 并行编译单元。

#### 5.1.5 代价

- **新增 crate 的管理开销**：Cargo.toml、release 流程、版本号维护。对一个 321 LOC 模块而言，crate 数量增长率不划算。
- **跨 crate refactor 摩擦**：未来要改某个段落 + 对应代码时，需要同时跨 crate 编辑。
- **依赖反向**：`peri-prompt` 依赖 `peri-middlewares`（吃 `AgentOverrides`），而 `peri-middlewares` 又依赖 `peri-agent`——若 `peri-agent` 反过来依赖 `peri-prompt`，会形成循环。需要明确 `peri-prompt` 是底层依赖。

### 5.2 方向 B：迁入 `peri-agent` 作为子模块

#### 5.2.1 目录结构

```
peri-agent/
├── src/
│   ├── session/
│   │   ├── mod.rs
│   │   └── frozen.rs              # FrozenContext 已存在
│   ├── prompt/                    # 新增子模块
│   │   ├── mod.rs                 # build_system_prompt
│   │   ├── features.rs
│   │   ├── env.rs
│   │   └── prompt_test.rs
│   └── ...
└── sections/                      # 14 个 .md 迁入 peri-agent
    └── ...
```

#### 5.2.2 Interface 草案

```rust
// peri-agent/src/prompt/mod.rs
use peri_middlewares::AgentOverrides;

pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String { ... }

// peri-agent/src/session/frozen.rs —— 新增构造 helper
impl FrozenContext {
    pub fn build_system_prompt(
        cwd: &str,
        language: Option<&str>,
        plugin_agent_dirs: &[std::path::PathBuf],
        frozen_date: &str,
    ) -> String {
        let features = crate::prompt::PromptFeatures::detect();
        crate::prompt::build_system_prompt(
            None, cwd, features, plugin_agent_dirs,
            Some(frozen_date), language,
        )
    }
}
```

#### 5.2.3 迁移成本

| 项 | 成本 | 备注 |
|----|------|------|
| 物理移动 14 个 .md 到 `peri-agent/sections/` | 低 | `git mv` |
| 物理移动 321 LOC 代码到 `peri-agent/src/prompt/` | 中 | 调整 `include_str!` 路径 |
| `peri-acp` 改 import：`use peri_agent::prompt::build_system_prompt` | 低 | 2 处 |
| 处理依赖反向：`peri-agent` 当前是否依赖 `peri-middlewares`？ | **需核验** | 见下文 |

**依赖核验**：`AgentOverrides` 来自 `peri-middlewares`。如果 `peri-agent` 尚未依赖 `peri-middlewares`，则本方向会引入新依赖边（`peri-agent → peri-middlewares`），需要检查是否形成循环（`peri-middlewares → peri-agent` 已存在）。如果形成循环，**方向 B 直接否决**。

#### 5.2.4 收益

- **语义对齐**：`build_system_prompt` 与 `FrozenContext` 同 crate，data flow 从 "peri-acp 构造 → 注入 peri-agent" 变成 "peri-agent 内部自构"，符合设计文档原则 5 的扩展解读。
- **零新 crate**：不增加 workspace 复杂度。
- **SubAgent 透传更近**：`FrozenContext` 已在 peri-agent，构造与消费同 crate。

#### 5.2.5 代价

- **破坏原则 5 的字面意义**：设计文档明确 "Provider 配置独立——ACP 构建好后注入 agent"。SP 构建目前遵循同样的 "ACP 构建好后注入 agent" 模式（`FrozenContext.system_prompt` 由 peri-acp 注入）。方向 B 把构建移到 peri-agent，**ACP 失去对 SP 内容的掌控**——而 ACP 是唯一知道 cwd / plugin_agent_dirs / language 的层。
- **依赖循环风险**：见 5.2.3。
- **agent crate 膨胀**：peri-agent 当前职责是 ReAct 引擎，加入 SP 装配会让它的边界变宽。这与候选 3（中间件链迁回）方向一致，但需要一次性决策。

### 5.3 方向 C：保持现状 + 重命名 / 文档化 seam

#### 5.3.1 Interface 草案（不迁移，仅澄清）

```rust
// peri-acp/src/prompt/mod.rs（保持原位）
// 文件头 doc comment 改写为：
//! System prompt construction — agent behavior definition.
//!
//! 此模块定义 agent 行为（system prompt 段落拼接），物理上位于 peri-acp，
//! 语义上跨越 ACP 编排与 agent 行为两层。保留在 peri-acp 的理由：
//! 1. 需访问 cwd / plugin_agent_dirs / language（ACP session 独有信息）
//! 2. 产出后通过 FrozenContext 注入 peri-agent，符合设计原则 5
//! 3. 迁移会破坏 prompt cache 不变量（SP 字节结构变化即 cache 失效）
//!
//! Seem: 候选 6 引入 SystemPromptBuilder trait 后，trait 定义可放到
//! peri-agent，实现保留在 peri-acp——这是更安全的演进路径。
```

#### 5.3.2 配合动作

1. **doc comment 改写**（如上），明确"这是 seam，不是 bug"。
2. **配合候选 6**：引入 `SystemPromptBuilder` trait，trait 定义放 `peri-agent`（消费方），实现保留 `peri-acp`（生产方）。这样既保留 locality，又让 builder 可注入 fake 测试。
3. **新增快照测试**：在 `prompt_test.rs` 中加一条 `test_system_prompt_byte_snapshot`，把当前输出 hash 成 golden，任何字节级 diff fail。这把"prompt cache 不可变"从文档约束升级为 CI 约束。

#### 5.3.3 成本

| 项 | 成本 | 备注 |
|----|------|------|
| doc comment 改写 | 极低 | 1 次提交 |
| 新增快照测试 | 低 | 单测，golden 文件入仓 |
| 配合候选 6 抽 trait | 中 | 独立候选，可分开推进 |

#### 5.3.4 收益

- **零迁移风险**：不动 `include_str!` 路径，不破坏 cache。
- **零依赖变化**：不引入新 crate，不形成循环。
- **保留 cwd/plug_dirs/language 的 locality**：ACP 层独有信息无需跨 crate 传递。
- **可演进**：trait 抽取后，未来若强证据出现，仍可走方向 A/B。

#### 5.3.5 代价

- **seam 仍在**：物理位置与语义责任不对齐，未来贡献者可能再次困惑。
- **不能独立复用**：其他 agent 实现想用 SP 装配，必须依赖整个 `peri-acp`。

### 5.4 推荐方向 + 论证

**推荐方向 C（保持现状 + 文档化 seam + 快照测试守护 cache）**，并配合候选 6 的 trait 抽取作为长期演进路径。

#### 5.4.1 推荐理由（按权重排序）

1. **Prompt cache 不变量是高代价约束**。SP 字节级稳定是 cache 命中率的基础，任何迁移（即便"等价重写"）都引入隐性 diff 风险（`include_str!` 路径解析、拼接顺序、空白处理）。**没有功能 bug 驱动的情况下，不值得冒这个险**——这是 Speculative 候选的核心否决理由。

2. **locality 实际上支持现状**。`build_system_prompt` 需要的输入（`cwd` / `plugin_agent_dirs` / `language`）只有 ACP session 层知道。方向 B 把构建移到 peri-agent 后，ACP 仍需把这些参数传过去——只是把"ACP 构造 → 注入 FrozenContext"变成"ACP 传参 → peri-agent 构造"，**没有简化数据流**，反而增加了一次跨 crate 调用。

3. **方向 A（独立 crate）的 ROI 不划算**。321 LOC 模块单独成 crate，管理开销（Cargo.toml / 版本 / workspace 注册 / 编译单元）超过收益。当前 `peri-acp` 编译时间未成为瓶颈（候选 6 才是 testability 瓶颈）。

4. **trait 抽取更对症**。本候选的真正痛点不是"位置不对"，而是"不可测"。候选 6 引入 `SystemPromptBuilder` trait 后，trait 定义可放 `peri-agent`（消费侧接口），实现保留 `peri-acp`（生产侧）——这是更轻、更安全的演进。

5. **方向 B 的依赖循环风险**。`AgentOverrides` 来自 `peri-middlewares`，`peri-middlewares` 又依赖 `peri-agent`。把 prompt 模块移到 peri-agent 会要求 `peri-agent → peri-middlewares`，形成循环依赖，**直接否决方向 B**（除非先重构 `AgentOverrides` 移出 peri-middlewares，这是另一个独立大工程）。

#### 5.4.2 触发迁移的条件（未来如出现，重审本候选）

| 触发条件 | 推荐重审方向 |
|---------|------------|
| 第二个 agent 实现（非 ACP 入口）需要复用 SP 装配 | 方向 A |
| peri-acp 编译时间显著恶化，需要拆分 | 方向 A（独立 crate 减负） |
| 候选 3（中间件链迁回 peri-agent）落地，agent 行为定义已大量在 peri-agent | 方向 B（语义一致性） |
| SP 装配逻辑膨胀到 1000+ LOC，需要独立版本管理 | 方向 A |
| 出现因 SP 构造位置导致的真实 bug | 重新评估 |

---

## 6. seam 后面剩什么

### 6.1 方向 A 下（独立 crate）

**`peri-prompt` 承担**：

- `build_system_prompt` 全部逻辑（136 LOC 函数体）
- `PromptFeatures` / `PromptEnv` 类型与 detect
- `build_agent_overrides_block` / `format_available_agents` / `os_version_string` / `map_language_to_instruction`
- 14 个 `.md` 段落文件（物理位置 `peri-prompt/sections/`）
- 27 条单测 + 新增字节级快照测试

**`peri-acp` 仍承担**：

- `session/executor.rs::FrozenSessionData::build` 调用 `peri_prompt::build_system_prompt`，构造 `FrozenContext`
- `frozen.rs` 的薄包装
- 把 `cwd` / `plugin_agent_dirs` / `language` / `frozen_date` 从 session 层传给 `peri-prompt`

**`peri-agent` 承担**：

- `FrozenContext` 类型（已存在）
- SubAgent frozen 数据透传（已存在）

### 6.2 方向 B 下（迁入 peri-agent）

**`peri-agent` 承担**：

- `prompt/` 子模块全部代码
- 14 个 `.md` 段落（物理位置 `peri-agent/sections/`）
- 27 条单测
- `FrozenContext` + `build_system_prompt` 同 crate，data flow 内聚

**`peri-acp` 仍承担**：

- `session/executor.rs` 把 `cwd` / `plugin_agent_dirs` / `language` 传给 `peri_agent::prompt::build_system_prompt`
- `FrozenSessionData` 装配（仍是 ACP session 概念）

**`peri-middlewares` 不变**：仍提供 `AgentOverrides`、`scan_agents_detailed`、`SkillsMiddleware::build_frozen_summary`。

### 6.3 方向 C 下（推荐：保持现状）

**`peri-acp` 继续承担**：

- `prompt/mod.rs` 321 LOC（不变）
- 14 个 `.md` 段落（`peri-acp/prompts/sections/`，不变）
- 27 条单测 + 新增字节级快照测试
- doc comment 明确"这是 agent 行为定义，但因 cache/locality 约束保留在 peri-acp"

**`peri-agent` 承担**：

- `FrozenContext` 类型（消费方）
- 若配合候选 6，新增 `SystemPromptBuilder` trait 定义（消费侧接口）

**14 个 `.md` 段落的物理位置**：保持 `peri-acp/prompts/sections/`。这保证 `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/sections/..."))` 路径稳定，零迁移风险。

---

## 7. 测试面

### 7.1 prompt_test.rs（491 LOC，27 测试）是否随模块迁移

| 方向 | 测试迁移决策 |
|------|----------|
| A（独立 crate） | **全部迁移**到 `peri-prompt/tests/` 或 `peri-prompt/src/prompt_test.rs`，保持 `#[path]` 模式 |
| B（迁入 peri-agent） | **全部迁移**到 `peri-agent/src/prompt/prompt_test.rs` |
| **C（推荐：保持现状）** | **不迁移**。测试与代码同位，零风险 |

无论哪个方向，**27 条测试必须全部继续通过**，特别是 5 条 boundary marker 守护测试（见 §2.4）。

### 7.2 prompt cache 不变量的守护

当前依赖文档约束 + 5 条 boundary marker 测试。**方向 C 推荐**新增一条字节级快照测试：

```rust
// prompt_test.rs 新增
#[test]
fn test_system_prompt_byte_snapshot() {
    // golden 文件：commit 进仓库的 system_prompt 完整输出
    let golden = include_str!("../../../tests/golden/system_prompt_default.txt");
    let features = PromptFeatures::none();
    let result = build_system_prompt(
        None, "/tmp/test", features, &[], Some("2026-07-13"), None,
    );
    assert_eq!(result, golden, "system prompt 字节级变化会破坏 Anthropic prompt cache");
}
```

golden 文件入仓，任何字节级 diff（即便来自"等价重构"）会让 CI fail，**强制开发者有意识地更新 golden 并文档化 cache 版本变化**。

### 7.3 frozen 数据流的回归测试

`FrozenSessionData::build`（`executor.rs:143-188`）当前无独立单测。无论是否迁移 SP，应补充：

```rust
#[test]
fn test_frozen_session_data_system_prompt_consistency() {
    let frozen = FrozenSessionData::build(
        "/tmp/test", None, &[], &[], "2026-07-13",
    );
    // SP 必须包含 boundary marker
    assert!(frozen.system_prompt().contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));
    // frozen date 必须出现在 SP 动态段
    assert!(frozen.system_prompt().contains("2026-07-13"));
    // v2_frozen 字段必须与 v1 accessor 一致
    assert_eq!(frozen.system_prompt(), &*frozen.v2_frozen().system_prompt);
}
```

这条测试与方向无关，是当前缺失的回归守护。

---

## 8. 风险与回滚

### 8.1 prompt cache 破坏风险（最高）

**风险**：任何 SP 字节级变化破坏 Anthropic Prompt Cache，命中率下降 → token 成本上升 → 用户体验退化（首 token 延迟增加）。

**守护**：

1. 方向 C 完全规避（不动代码）。
2. 若走 A/B，必须**先**在旧位置生成 golden snapshot，**再**迁移，**再**断言字节级相等。
3. 迁移 PR 必须包含 cache 命中率对比（langfuse trace 前/后各 50 次会话）。

**回滚**：单次 `git revert`，无数据迁移。

### 8.2 frozen data 透传链路风险

**风险**：迁移后 `FrozenSessionData::build` 调用点（`executor.rs:163` / `executor.rs:879`）的 import 路径变化，若遗漏一处，编译失败（编译期捕获，低风险）；若 SubAgent 透传链路有隐性假设（如某处直接读 `build_system_prompt` 而非走 frozen），运行时行为变化。

**守护**：

1. `grep -rn "build_system_prompt" peri-acp/src/` 确认全部调用点（当前 5 处：`prompt/mod.rs` 定义、`session/executor.rs` 2 处、`agent/builder.rs`、`agent/workflow_agent.rs`、`prompt/prompt_test.rs`）。
2. SubAgent 侧（`peri-middlewares/src/subagent/`）必须只读 `FrozenContext.system_prompt`，不直接调 `build_system_prompt`——当前已遵守，迁移后需回归测试 SubAgent 行为。

**回滚**：单次 `git revert`。

### 8.3 跨 crate 引用断裂

**风险**：方向 B 引入 `peri-agent → peri-middlewares` 依赖（吃 `AgentOverrides`），若与 `peri-middlewares → peri-agent` 形成循环，编译失败。

**守护**：迁移前 `cargo tree -p peri-agent -e features` 核验依赖图。

**回滚**：单次 `git revert`。

### 8.4 测试迁移导致覆盖率瞬时下降

**风险**：若 `git mv` 测试文件时遗漏，CI 显示覆盖率波动，可能掩盖真实的代码迁移 bug。

**守护**：迁移 PR 必须显示 `cargo test -p <target_crate> --lib` 全绿 + 27 条测试全部存在（`grep -c "#\[test\]"`）。

### 8.5 doc comment 与实际行为漂移

**风险**：方向 C 下，仅改 doc comment，但 doc comment 与代码行为可能随时间漂移。

**守护**：doc comment 引用具体行号与候选编号（如 `参见候选 7 §5.4`），让未来贡献者能追溯决策上下文。

---

## 9. 迁移步骤

### 9.1 推荐方向 C 的执行步骤（轻量）

本候选**不推荐迁移**，因此 §9.1 给出"为何不动 + 配合动作"的明确论证。

#### 9.1.1 为何不动（强理由汇总）

1. **没有 bug 驱动**。Speculative 候选的核心标准是"有强证据才动"。当前 prompt 模块测试覆盖良好（27 测试）、零外部副作用、零生产事故。**"位置不对"不是动它的理由，"出 bug"才是**。

2. **prompt cache 不变量是高压线**。SP 字节级稳定是 cache 命中率基础。任何"等价重构"都可能引入隐性 diff（路径解析、空白、拼接顺序）。**cache 命中率退化是隐性回归**，可能数周后才被发现，且需要 trace 级工具才能定位。

3. **locality 实际支持现状**。`build_system_prompt` 的关键输入（`cwd` / `plugin_agent_dirs` / `language`）只有 ACP session 层知道。迁移到 peri-agent 后，ACP 仍需把这些参数跨 crate 传过去，**没有简化数据流**。

4. **方向 B 依赖循环**。`AgentOverrides` 在 `peri-middlewares`，方向 B 要求 `peri-agent → peri-middlewares`，与现有 `peri-middlewares → peri-agent` 形成循环，**直接否决**。

5. **方向 A 的 ROI 不划算**。321 LOC 模块单独成 crate，管理开销（Cargo.toml / 版本 / workspace 注册）超过收益。`peri-acp` 编译时间未成为瓶颈。

6. **trait 抽取更对症**。本候选的真正痛点是"不可测"，候选 6 引入 `SystemPromptBuilder` trait 即可解决，trait 定义放 `peri-agent`、实现留 `peri-acp`，**零迁移风险**。

#### 9.1.2 配合动作（落地清单）

| # | 动作 | 代价 | 优先级 |
|---|------|------|-------|
| 1 | 改写 `prompt/mod.rs` 头 doc comment，明确 seam 性质 | 1 次提交 | P0 |
| 2 | 新增 `test_system_prompt_byte_snapshot` 快照测试 + golden 文件 | 1 次提交 | P0 |
| 3 | 新增 `test_frozen_session_data_system_prompt_consistency` 回归测试 | 1 次提交 | P1 |
| 4 | 推进候选 6，引入 `SystemPromptBuilder` trait，trait 定义放 peri-agent | 独立候选 | P1 |
| 5 | 在 `CLAUDE.md` 模块索引中标注 prompt 模块的 seam 性质 | 1 行 | P2 |

#### 9.1.3 重审触发条件

- 第二个 agent 实现需要复用 SP 装配 → 重审方向 A
- 候选 3（中间件链迁回）落地 → 重审方向 B
- peri-acp 编译时间恶化 → 重审方向 A
- SP 装配逻辑膨胀到 1000+ LOC → 重审方向 A

### 9.2 如果未来走方向 A 的分阶段计划（参考）

仅在重审触发条件出现时执行：

**阶段 1（准备）**：

- 在旧位置生成 golden snapshot（系统提示词完整输出）
- 把 `test_system_prompt_byte_snapshot` 加入 CI

**阶段 2（建 crate）**：

- 新建 `peri-prompt` crate，注册到 workspace
- `git mv` 14 个 `.md` 段落到 `peri-prompt/sections/`
- 复制（非移动）`prompt/mod.rs` 代码到 `peri-prompt/src/`，拆分到子模块
- 复制 `prompt_test.rs`，调整 import

**阶段 3（切换）**：

- `peri-acp` 改 import：`use peri_prompt::build_system_prompt`
- 删除 `peri-acp/src/prompt/` 与 `peri-acp/prompts/sections/`
- CI 全绿 + 字节级快照通过

**阶段 4（验证）**：

- langfuse trace 对比 cache 命中率（前/后各 50 次会话）
- 命中率无显著下降（< 5% 波动视为环境噪声）

---

## ADR 草案

### ADR-2026-07-13-system-prompt-location

**标题**：System Prompt 构造模块（`build_system_prompt`）的物理位置决策

**状态**：Proposed

**上下文**：

`peri-acp/src/prompt/mod.rs:116-252` 的 `build_system_prompt()` 是 peri-acp 中最深的纯逻辑模块（321 LOC + 491 LOC 测试），它定义的是 agent 行为（system prompt 段落拼接），语义上属于 peri-agent 范畴。然而它物理上位于 peri-acp，原因是其输入（`cwd` / `plugin_agent_dirs` / `language`）只有 ACP session 层知道，且其产出通过 `FrozenContext.system_prompt` 注入 peri-agent，符合设计原则 5（"ACP 构建好后注入 agent"）。

本候选评估三个方向：A（独立 `peri-prompt` crate）/ B（迁入 `peri-agent`）/ C（保持现状 + 文档化）。

**决策**：**不迁移，保持现状（方向 C）**。

**动机**：

1. **Prompt cache 不变量是高压线**。SP 字节级稳定是 Anthropic Prompt Cache 命中率基础。"等价重构"引入的隐性 diff（路径解析、空白、拼接顺序）可能导致 cache 命中率退化，且难以即时发现。没有 bug 驱动的情况下，不值得冒这个险。

2. **locality 实际支持现状**。`build_system_prompt` 的关键输入只有 ACP session 层知道，迁移后仍需跨 crate 传参，不简化数据流。

3. **方向 B 依赖循环**。`AgentOverrides` 在 `peri-middlewares`，方向 B 要求 `peri-agent → peri-middlewares`，与现有反向依赖形成循环，直接否决。

4. **方向 A ROI 不划算**。321 LOC 模块单独成 crate 的管理开销超过收益。

5. **trait 抽取更对症**。真正痛点是"不可测"而非"位置不对"，候选 6 引入 `SystemPromptBuilder` trait 即可解决，零迁移风险。

**后果**：

- **正面**：
  - 零 prompt cache 破坏风险
  - 零依赖图变化
  - 零 crate 管理开销
  - locality 保留（ACP session 独有信息无需跨 crate 传递）
  - 配合候选 6 后仍可演进
- **负面**：
  - 物理位置与语义责任不对齐的 seam 仍在
  - 未来贡献者可能再次困惑（通过 doc comment + 本 ADR 缓解）
  - 其他 agent 实现无法直接复用 SP 装配（通过 trait 抽取或重审方向 A 缓解）

**重审触发条件**：

- 第二个 agent 实现需要复用 SP 装配
- 候选 3（中间件链迁回 peri-agent）落地
- peri-acp 编译时间显著恶化
- SP 装配逻辑膨胀到 1000+ LOC
- 出现因位置导致的真实 bug

**配合动作**：

1. 改写 `prompt/mod.rs` 头 doc comment，明确 seam 性质（P0）
2. 新增 `test_system_prompt_byte_snapshot` 快照测试 + golden 文件（P0）
3. 新增 `test_frozen_session_data_system_prompt_consistency` 回归测试（P1）
4. 推进候选 6，trait 定义放 peri-agent，实现留 peri-acp（P1）
5. 在 `CLAUDE.md` 模块索引中标注 prompt 模块的 seam 性质（P2）

**参考**：

- `docs/design/peri-agent-system-prompt-v2.md`（SP v2 设计原则）
- `docs/design/peri-agent-acp-v2.md` §1.5（Provider 配置独立原则）
- `docs/blogs/prompt-cache-optimization/prompt-cache-optimization.md`（cache 优化背景）
- `CLAUDE.md`（frozen data flow 与陷阱速查）
- 候选 6（trait 抽取让 build_system_prompt 可测）

---

> **文档版本**：v1.0 | **行数**：约 700 行 | **流程**：/grilling（Speculative seam 错层深化） | **决策**：保持现状（方向 C） + ADR 记录
