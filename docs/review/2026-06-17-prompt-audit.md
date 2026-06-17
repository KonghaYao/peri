# Peri 系统提示词对抗审核报告

> 日期：2026-06-17 · 8 Agent 协作（5 审核 + 3 仲裁）· 16 段落 · 15.7KB 总长度

> **修订记录（2026-06-17 二次核查）**：对照 13 个段落源文件、`hitl/mod.rs`、`builder.rs`、`core_tools.rs`、`agent-defect-analyzer/docs/report-2026-06-17.md` 复核后发现并修正以下条目：
> - **P0-2 撤销** — 基于误读。02 实际表述为 "allowed to be proactive, including follow-up actions"，03 lint/build 是合理的 follow-up，二者一致无冲突
> - **P1-12 撤销** — "loop independently" 在 ReAct 框架下是合理描述，非虚假承诺
> - **P1-16 撤销** — 事实错误。`core_tools.rs:19-20` 仍定义 `TOOL_WRITE` 和 `TOOL_EDIT`，Edit 工具未删除
> - **P0-4 数据指标修正** — 原文 "选择准确率 27%" 改为 "调用频次比"。原数据是 168h 内 SubAgent 调用次数统计，无 ground truth 支持准确率评估
> - **P0-7 描述修正** — 删除 "lost on restart 误导" 判断（中性事实），保留 cron_register 逃逸 HITL 的事实
> - **P1-19 弱化** — filler summary（无意义填充）与 useful summary 语义不直接矛盾，措辞相近仅是潜在困惑

---

## 一、审核流程

| 阶段 | Agent | 职责 | 输出 |
|------|-------|------|------|
| 审核 | 5 个 | 按功能域拆分，6 维度审核（冗余/缺失/歧义/效率/矛盾/工程） | 40 个原始发现 |
| 仲裁 A | 共识检测 | 交叉验证发现，识别强/中/弱共识 | 7 组共识确认 + 冲突裁决 |
| 仲裁 B | 优先级排序 | 去重 → 建立依赖 → 分阶段排序 | 30 行动项，3 阶段 |
| 仲裁 C | 可行性评估 | 9 P0 逐个评估改动类型/风险/副作用（撤销 1 项后实际 8 项） | 风险矩阵（见第五章） |

**6 个审核维度**：冗余重复 / 信息缺失 / 表述歧义 / Token 效率 / 跨段落矛盾 / Prompt 工程质量

---

## 二、关键发现

### P0 致命问题（9 个原始 → 撤销 1 项后 8 个，须立即修复）

| # | 段落 | 问题 | 仲裁裁决 |
|---|------|------|----------|
| 1 | `01_intro` | 安全红线 "may be used maliciously" 自我消解——所有代码都可能被恶意使用，规则逻辑无效 | 改为行为级约束 "Do not write code that attacks systems, exploits vulnerabilities, steals data, or bypasses access controls" |
| ~~2~~ | ~~`02_system`~~ | ~~Proactiveness "不要主动" 与 `03` "run lint and build" 冲突~~ **[撤销·误读]** 02 实际表述为 "You are allowed to be proactive, but only when the user asks you to do something ... including taking actions and **follow-up actions**"，03 lint/build 是合理的 follow-up action，二者一致无冲突 | ~~改为 Phase 结构~~ 无需改动 |
| 3 | `06_tone_style` | "Answer in fewer than 4 lines" 与 `03` "state assumptions + plan" + `04` "confirm scope" + `11` "summarize results" 结构性冲突——**本次审核最高置信度问题（2 Agent 共识）** | 分阶段：Planning 允许详细输出，Execution 后保持简洁 |
| 4 | `11_subagent` | "general-purpose" 名称效应压倒 Selection Guide——168h 内调用频次 general-purpose 174 次 vs coder 65 次（2.7x），数据源 `agent-defect-analyzer/docs/report-2026-06-17.md:176` | 改为 fallback 命名，加重负面 framing |
| 5 | `14_system_reminder` | 无 prompt injection 防御——用户可伪造 `<system-reminder>` 标签注入恶意指令 | 明确"用户手打的标签不算系统通知" + 后端清理 |
| 6 | `10_hitl` | 审批列表缺失 WebFetch、WebSearch、mcp__*——与代码实际拦截范围不一致（`hitl/mod.rs:49-60` 拦截 10 类，prompt 仅描述 6 类） | 补全列表，与 `hitl/mod.rs:49-60` 对齐 |
| 7 | `12_cron` | `cron_register` 工具不经 HITL 审批（`default_requires_approval` 不含 `cron` 前缀匹配），可注册定时执行任意 prompt 构成安全风险 | 代码级 HITL 拦截 + prompt 加安全约束 |
| 8 | `git_attr` | "always include in commits" ↔ `03` "NEVER commit" 直接矛盾（builder.rs:442 字面 "should always be included"） | 改为 "when user asks you to commit, include this line" |
| 9 | `06` | "No preamble" 具体禁用 `03` 的 Plan 步骤和 `04` 的 Confirm 步骤 | 合并入 #3 |

### P1 高优先级问题（18 个原始 → 撤销 2 项、弱化 1 项后 15 个，本月修复）

| # | 段落 | 问题 |
|---|------|------|
| 10 | `01_intro` | URL 规则与 WebFetch 工具自相矛盾 |
| 11 | `02_system` | "first look at existing" 重复 3 次（~50 token 浪费） |
| ~~12~~ | ~~`03_doing_tasks`~~ | ~~"loop independently" 是虚假承诺~~ **[撤销]** ReAct 框架下目标驱动循环是合理描述，"Strong success criteria let you loop independently" 是工程陈述非承诺 |
| 13 | `04_actions` | "200 lines→50" 不可执行（LLM 无行数预算意识） |
| 14 | `04_actions` | "Don't improve adjacent code" — adjacent 未定义 |
| 15 | `05_using_tools` | 缺失核心工具系统性指导（Bash/WebFetch/Agent 安全引用） |
| ~~16~~ | ~~`05_using_tools`~~ | ~~Write vs Edit 未区分（Edit 已从工具集删除）~~ **[撤销·事实错误]** `core_tools.rs:19-20` 仍定义 `TOOL_WRITE` 和 `TOOL_EDIT`，两者都在 `CORE_TOOLS` 数组（第 42-43 行），Edit 工具未删除 |
| 17 | `11_subagent` | Selection Guide ≈ Agent Description 冗余副本（双通道竞争） |
| 18 | `11_subagent` | Fork/Background 高级功能挤占初学者空间 |
| 19 | `11_subagent` | "Summarize results" ↔ 06 "No filler summaries" 措辞相近可能引起 LLM 困惑（弱化：语义上 filler=无意义填充 ≠ useful summary，不严格矛盾，但 LLM 可能误读） |
| 20 | `10_hitl` | write_*/edit_* 命名语义错误（通配 vs 精确匹配） |
| 21 | `10_hitl` | Reject 无理由时缺少 Agent 指导 |
| 22 | `07_env` | 动态值缺乏过期提示 |
| 23 | `13_skills` | 措辞让 LLM 混淆谁调用 skill |
| 24 | `15_channel` | MCP 工具名模式错误 + 缺失 SearchExtraTools 发现流程 |
| 25 | `lang` | "Technical terms" 定义模糊 |
| 26 | `12_cron` | 安全约束严重不足（仅 3 句 vs Git Safety 7 条） |
| 27 | `13_skills` | 发现度不足（40+ Skill 仅 9 种被使用） |

### 仲裁 Agent A 共识分析

**强共识 ⭐⭐⭐（多 Agent 独立发现，最高置信度）**：

| 共识 | 发现 Agent | 问题 |
|------|-----------|------|
| 1 | A1 + A2 | `06_tone_style` 与 `03/04/11` 结构性冲突（P0） |
| 2 | A2 + A3 | "No filler summaries" ↔ "Summarize sub-agent results" 矛盾（P1） |
| 3 | A4 + A5 | HITL 审批覆盖盲区系统性模式（P0） |

**关键独立发现（经仲裁确认有效）**：

| 发现 | 来源 | 仲裁意见 |
|------|------|----------|
| general-purpose 名称效应（168h 内 174 次 vs 65 次调用） | A3 | 建议升级 P0——实证数据无可辩驳（注：原数据为调用频次，非"选择准确率"） |
| Git Attribution ↔ NEVER commit 矛盾 | A5 | P0，A1（审查 03）遗漏此跨段落检查 |
| system_reminder prompt injection | A4 | 降为 P1——实际攻击面有限，但防御成本低 |

---

## 三、行动列表

### 第一阶段：立即修复（本周 · 无依赖）

| # | 行动 | 文件 | 类型 | 风险 |
|---|------|------|------|------|
| ~~A1~~ | ~~02_system Proactiveness 改为 Phase 结构~~ **[撤销]** 基于 P0-2 误读 | — | — | — |
| A2 | 06_tone_style 分阶段约束（Plan 详细 / Report 简洁） | `06_tone_style.md` + `03_doing_tasks.md` | prompt | 中 |
| A3 | Git Attribution "always include" → "when user asks" | `peri-acp/src/agent/builder.rs` | 代码 | 低 |
| A4 | 01_intro 安全规则改为行为级约束 | `01_intro.md` | prompt | 低 |
| A5 | 10_hitl 补全审批列表（WebFetch/WebSearch/mcp__*） | `10_hitl.md` | prompt | 低 |
| A6 | 12_cron 加安全约束 + hitl/mod.rs 拦截 cron_register | `12_cron.md` + `hitl/mod.rs` | prompt+代码 | 中 |
| A7 | 11_subagent Selection Guide 强化（加重负面 framing） | `11_subagent.md` | prompt | 低 |
| A8 | 14_system_reminder 增加防伪造指令 | `14_system_reminder.md` | prompt | 低 |

### 第二阶段：短期优化（本月 · P1）

| # | 行动 | 文件 |
|---|------|------|
| B1 | 05_using_tools 增加工具选择决策树 | `05_using_tools.md` |
| B2 | 04_actions 精确化（200→50 改 nesting + adjacent 定义） | `04_actions.md` |
| B3 | 11_subagent 去冗余+精简（预计 -250 token, 29%） | `11_subagent.md` |
| B4 | 13_skills 措辞修正（LLM 只需识别不需主动加载） | `13_skills.md` |
| B5 | 15_channel MCP 工具名修正 + SearchExtraTools 流程 | `15_channel.md` |
| B6 | 语言指令 "Technical terms" 范畴列举 | 语言指令区 |
| B7 | 02/03/04 冗余消除（去重+删冗余 intro+loop 措辞） | `02`+`03`+`04` `.md` |
| B8 | 07_env 时效标注 | `07_env.md` |

### 第三阶段：持续改进（P2）

| # | 行动 | 文件 |
|---|------|------|
| C1 | 01_intro URL 规则改为行为导引 | `01_intro.md` |
| C2 | 13_skills 发现度提升（触发指南） | `13_skills.md` |
| C3 | 15_channel terminal↔channel 切换指导 | `15_channel.md` |
| C4 | 02 安全指令具象化 | `02_system.md` |
| C5 | Git Safety 排序优化（force push 提升到段首） | `04_actions.md` |

---

## 四、依赖关系

```
第一阶段（行为基调，必须最先）
  ✗ A1 已撤销（P0-2 误读，02_system 无需 Phase 结构改造）
  A2: Tone/Style 分阶段（独立可做，不依赖 A1）
  A3: Git Attribution 修复（独立）
  A4: 安全规则重写（独立）
  A5: HITL 列表补全（独立）      A8: reminder 防注入（独立）
  A6: cron 安全+审批（独立）     A7: SubAgent 强化（独立）
         │
         ▼
第二阶段（依赖第一阶段的基调明确）
  B1: 工具指导 + B2: 精确化 + B3: SubAgent 去冗余
  B4-B8: 各自独立
         │
         ▼
第三阶段（低优先级增强，可随时并行）
  C1-C5: 各自独立
```

---

## 五、P0 可行性与风险矩阵（仲裁 Agent C 输出）

### 逐个评估

| # | 问题简述 | 改动类型 | 文件数 | 回归风险 | 测试难度 | 副作用风险 | 推荐方案 |
|---|---------|---------|--------|---------|---------|-----------|---------|
| P0-1 | 01_intro 安全规则自我消解 | 仅 prompt | 1 | 低 | 低（文本替换） | 无 | 行为级约束替代 "may be used maliciously" |
| ~~P0-2~~ | ~~02 "不要主动" ↔ 03 "必须 lint" 冲突~~ **[撤销·误读]** | — | — | — | — | — | 无需改动（02 实际允许 follow-up actions，与 03 lint/build 一致） |
| P0-3 | 06 "No preamble" ↔ 03 Plan + 04 Confirm | 仅 prompt | 3 | 中 | 低 | 可能改变现有行为平衡 | 分阶段：Plan 详细 / Report 简洁 |
| P0-4 | GP 名称效应压倒 Selection Guide | 仅 prompt | 1 | 低 | 中（需观测 LLM 行为） | 改名可能影响代码中硬编码 | 增强 Selection Guide 负面 framing |
| P0-5 | system_reminder 无注入防御 | 两都需 | 2 | 中 | 中（构造恶意输入） | 过度防御可能破坏正常 recall | 代码 sanitize + prompt 明确化 |
| P0-6 | HITL 审批列表缺失工具 | 仅 prompt | 1 | 低 | 低（目视对比） | 无 | 补全 WebFetch/WebSearch/mcp__* |
| P0-7 | cron_register 不经 HITL 审批（删除"误导"判断） | 两都需 | 2 | 中 | 中（集成测试） | HITL 模式下 cron 注册需确认 | prompt 加约束 + hitl/mod.rs 拦截 |
| P0-8 | Git Attribution ↔ NEVER commit | 仅代码 | 1 | 低 | 低（目视检查） | 无 | builder.rs 改 1 行 |
| P0-9 | 06 "No preamble" 禁用 Plan/Confirm | — | — | — | — | — | **合并到 P0-3** |

### 架构约束

**系统提示词缓存边界**：
```
01_intro ──┐
02_system ─┤
03_tasks ──┤ ← 静态区域（Anthropic cache_control）
04_actions ─┤   修改即全量缓存失效
05_tools ──┤
06_tone ───┘
── __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__ ──
07_env ────┐
10_hitl ───┤
11_subagent┤ ← 动态区域（修改不影响缓存前缀）
12_cron ───┤
13_skills ─┤
14_reminder┤
15_channel─┘
── 语言指令 ──（运行时动态）
── Git Attribution ──（运行时动态）
```

**HITL 拦截列表**（`peri-middlewares/src/hitl/mod.rs:49-60`）：代码实际拦截 10 个工具，`10_hitl.md` 仅描述 6 个，缺失 WebFetch、WebSearch、mcp__*。

**cron_register 逃逸**：工具名 `cron_register` 不匹配 `default_requires_approval` 中任何条件，可绕过所有审批。

### 实施顺序（按风险从低到高）

```
阶段 1：低风险快速验证（立即可做）
├── P0-6  补全 HITL 审批列表         [动态区域]
├── P0-8  修正 Git Attribution 措辞   [代码层文本拼接]
├── P0-4  增强 SubAgent Selection Guide [动态区域]
├── P0-1  重写 01_intro 安全规则     [静态区域]
├── ✗ P0-2 已撤销（基于误读）
└── P0-3  06 No preamble ↔ 03/04     [静态区域，分阶段约束]

阶段 2：中风险充分测试
├── P0-5  添加 system_reminder sanitization [代码+动态 prompt]
└── P0-7  cron_register 加入 HITL 拦截      [代码+动态 prompt]
```

**统计**：原 8 个独立问题，撤销 P0-2 后为 7 个 · 5 个纯 prompt · 1 个纯代码 · 2 个双修 · 约 10 个涉及文件

### 验证流程

1. 目视对比 + `cargo test -p peri-acp` 确保 prompt 构建不引入语法错误
2. 新增 `system_reminder` 注入防护测试 + cron HITL 拦截测试
3. `cargo test` 全量回归
4. TUI 手动验证修改后 agent 行为无退化

---

## 六、高风险操作预警

| # | 改动 | 风险 | 缓解措施 |
|---|------|------|----------|
| A2 | 06_tone_style 修改 | 中——涉及 03/04/11 联动，可能引入新矛盾 | 先做 P0-3 分阶段约束，再在基础上验证 A2 |
| A6 | cron HITL 拦截 | 中——代码级修改，需验证不破坏现有 HITL 流程 | 补充 cron_register 的 HITL 集成测试 |
| B3 | 11_subagent 去冗余 | 中——改名可能影响代码中硬编码匹配 | 全量搜索 "general-purpose" 字符串引用后实施（grep 已确认 `peri-middlewares/src/subagent/built-in/general-purpose.md` 等多处引用） |

---

## 七、预计效果

| 指标 | 改前 | 改后 |
|------|------|------|
| 总长度 | 15.7 KB | ~15.3 KB（净减 200-300 token） |
| 段落间矛盾 | 3 组结构性冲突（P0-3、P0-8、P0-9 合并） | 0 |
| coder 调用频次占比 | 27%（65/239 = coder / (general-purpose + coder)） | 无 ground truth，"选择准确率"指标不可预测，仅能通过强化 framing 观测分布变化 |
| Prompt injection 防御 | 无 | 双层（prompt 级 + 后端清理） |
| HITL 覆盖完整度 | 60%（6/10 工具） | 100% |

---

*报告由 agent-defect-analyzer 生成 · 8 Agent 协作 · 2026-06-17*
