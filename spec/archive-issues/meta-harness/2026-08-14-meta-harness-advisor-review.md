# MetaHarness advisor 红队复审——Q1/Q2/Q5/Q10 决策记录

**状态**：Closed（决策已执行）
**优先级**：高（推翻两项已拍板裁定）
**类型**：决策记录
**创建日期**：2026-08-14
**来源**：用户要求"派出 /advisor-consultation 对抗刚刚的决定"（Q1-Q10 实现偏差
裁定评审）；advisor 输出 + 代码事实核查后由用户拍板，全部"遵照 advisor 的"。

## 背景

Q1-Q10 实现偏差裁定评审后，advisor-consultation 对其中四项提出反对/质疑。
本记录归档复审结论与执行结果；设计文档 `docs/design/meta-harness.md`
已同步（状态行 + 2.4/3.1/3.3/2.7/2.8）。

## 复审结论

### Q1（推翻，已执行）——PromptLayer 去除

- **advisor 论点**：保留 Layer 与用户"层概念全部去除"早期约束冲突；
  "Layer 是位置属性演进基础"的理由被代码驳斥——PromptLayer 是内容分类
  （SafetyAuthorization / EngineeringBehavior / CapabilityContract /
  RuntimeStateBoundary / PersonaDomain），位置由数组归属决定。
- **事实核查**：成立。`ResolvedSection.layer` 为 `#[allow(dead_code)]`
  纯元数据（prompt/mod.rs），渲染只消费 `content`；无任何逻辑分支依赖 Layer。
- **决策**：采纳。删除 `PromptLayer` 枚举、数组第三/四元素、
  `ResolvedSection.layer` 字段；`IMMUTABLE_SECTIONS` /
  `ALWAYS_UNCACHED_SECTIONS` 降为 (ID, 内容) 二元组、`GATED_SECTIONS`
  降为 (ID, 内容, Gate) 三元组；测试同步（`test_workflow_gate_positioned_in_gated_sections`）。

### Q2（推翻实现、保留目标，已执行）——SectionContent 零拷贝双态

- **advisor 论点**：`Arc::from(builtin)` 对 `&'static str` 全量堆拷贝，
  "零复制"前提错误；建议 `Builtin(&'static str) / Override(Arc<str>)` 枚举。
- **事实核查**：成立（`resolve_section` 的 `Arc::from` 拷贝全文；13 段 ×
  每构造点，非热路径但可零成本改进）。
- **决策**：采纳 advisor 原案。`ResolvedSection.content` 改为
  `SectionContent` 枚举：`Builtin(&'static str)` 零拷贝借静态文本 /
  `Override(Arc<str>)` 持冻结期覆盖全文。

### Q5（驳回 advisor 推翻，已执行）——merge 生产线路已接线

- **advisor 论点**：merge 生产线路缺失 → 项目级配置可能根本不应用。
- **事实核查**：不成立。`store.rs:70-77` `load()`（全局 + 工作区双文件，
  `:74` 调用 `merge_overrides`）经 `peri-tui/src/config/mod.rs` re-export 后
  由 `peri-tui/src/app/mod.rs:53` 生产调用；meta_harness 逐 key 特例随生产
  线路生效。**文档 3.3 早期"无接线"记录有误**（advisor 依据了错误文档事实）。
- **决策**：驳回 advisor 推翻；修正文档 3.3 事实 + 新增契约测试
  `store_test::test_load_merges_meta_harness_per_key`（`#[serial]`，临时切
  cwd 走完整 `load()` 路径，锁定逐 key 合并经生产入口生效）。

### Q10（维持，证据成立）

- **advisor 要求**：冻结/装配时序证明。
- **证据链**：`build_frozen_data`（session/mod.rs:459）→
  `build_meta_harness_state` 纯函数（:710，合并配置 + 扫描结果一次构建）→
  `FrozenContext.meta_harness` 单字段（store.rs:69）→ 每 turn 装配经
  `SessionContext.meta_harness` 投影（executor.rs:406-410，注释明确"禁止从
  每 turn 当前配置重建"）→ `StageBuildInput.meta_harness_disabled` →
  `AssemblyContext.meta_harness_disabled`（stage_builder.rs:438）→ 全部装配
  入口过滤；空冻结回退 default（executor.rs:1190-1191）。冻结后宿主更新
  `peri_config` 不影响本会话（ARC-FROZEN-001 语义）。
- **决策**：维持。无代码改动。

## 附加：文档矛盾修复

- 3.1.1 旧文本"无独立 ID 字段"与三元/四元组表并存 → 已修复（ID 为数组
  元组结构字段，非独立 struct 字段；render 不依赖 ID）。

## 涉及文件

- `peri-acp/src/prompt/mod.rs` — Layer 去除 + SectionContent 枚举
- `peri-acp/src/prompt/prompt_test.rs` — 测试同步
- `peri-acp/src/provider/store_test.rs` — Q5 契约测试 + CwdGuard
- `docs/design/meta-harness.md` — 2.4/3.1/3.3/2.7/2.8/状态行同步

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Closed | agent | advisor 红队复审执行完毕：Q1 去除 Layer、Q2 SectionContent、Q5 驳回 + 契约测试、Q10 证据成立、文档矛盾修复；代码/测试/文档全部落地 |
