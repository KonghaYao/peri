# 候选 5：event/mapper 与 mapper_v2 命名错向，按方向重命名

> 日期：2026-07-13 | 模块：`peri-acp/src/event/mod.rs` + `mapper.rs` + `mapper_v2.rs` | 类型：架构走读
> 流程：/grilling（命名方向 + shallow re-export 删除测试）
> 范围：1 个 re-export 桥接文件（10 LOC）+ 1 个正向映射文件（509 LOC）+ 1 处 `pub use` 群

---

## 1. 摘要

`peri-acp/src/event/mod.rs:19-22` 同时对外暴露两个名字相近、数据流向相反的模块：`mapper` 把 `ExecutorEvent` 正向映射成 ACP 协议产物（`SessionUpdate` / `AcpEvent` / `RoutingOutput`），`mapper_v2` 把 v2 的 `Render/State/Observe` 事件反向桥接成 `ExecutorEvent`。两者都叫 "mapper"，但方向相反——新人读 `mod.rs` 时极难从 import 名字判断数据流。更严重的是，`mapper_v2.rs` 仅 10 行，全部内容是 `pub use peri_agent::agent::events_v2_mapper::{...}` 的 re-export，命名严重夸大了它的体量，且经过 grep 验证**没有任何下游 crate 消费这次 re-export**（`forwarder.rs` 走 `crate::event::mapper_v2::` 模块路径，不依赖 `pub use`）。

本候选走 /grilling 流程，逐一拷问三个加深方向（A 重命名 / B 内联删除 / C 注释加方向标）的 depth、leverage、locality，结论是 **方向 A（重命名为 `events_v2_bridge.rs`）+ 方向 C（给 `mapper.rs` 补文件头方向注释）组合为推荐方案**，因为重命名是机械重构、零语义风险、立即消除方向歧义；方向 B（内联删除）作为 Phase 2 可选项，依赖候选 4（dispatch 薄壳群合并）落地的 re-export 内联惯例，但 `mapper_v2` 的 case 比候选 4 列出的其它 re-export 更弱——它甚至没有外部消费者，理论上今天就能删。

---

## 2. 现状诊断

### 2.1 mod.rs 的双 `pub use` 块（方向错向根因）

`peri-acp/src/event/mod.rs:19-22`：

```rust
pub use mapper::{executor_event_to_acp, map_event, MappedEvent};
pub use mapper_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
```

两行紧邻，名字仅差一个 `_v2` 后缀，但数据流向完全相反：

| 模块 | 入参类型 | 出参类型 | 方向 |
|------|---------|---------|------|
| `mapper::map_event` | `&ExecutorEvent` | `Vec<MappedEvent>`（含 `SessionUpdate`） | ExecutorEvent → ACP 协议（正向） |
| `mapper::executor_event_to_acp` | `&ExecutorEvent` | `Option<AcpEvent>` | ExecutorEvent → AcpEvent DTO（正向，TUI 通道） |
| `mapper_v2::render_event_to_executor` | `RenderEvent`（v2） | `Option<ExecutorEvent>` | v2 → ExecutorEvent（反向桥接） |
| `mapper_v2::state_event_to_executor` | `StateEvent`（v2） | `Option<ExecutorEvent>` | v2 → ExecutorEvent（反向） |
| `mapper_v2::observe_event_to_executor` | `ObserveEvent`（v2） | `Option<ExecutorEvent>` | v2 → ExecutorEvent（反向） |

新人读 `use peri_acp::event::{map_event, render_event_to_executor}` 时，无法从符号名推断两者方向相反——前者从 ExecutorEvent 出去，后者回到 ExecutorEvent。这是典型的 interface 命名缺失方向标。

### 2.2 LOC 与体量对比（命名夸大问题）

```
$ wc -l peri-acp/src/event/{mapper,mapper_v2,router,forwarder}.rs
   509 peri-acp/src/event/mapper.rs
    10 peri-acp/src/event/mapper_v2.rs
   148 peri-acp/src/event/router.rs
    85 peri-acp/src/event/forwarder.rs
   752 total
```

`mapper_v2.rs` 全部内容（10 行，含 6 行注释）：

```rust
//! v2 事件 → v1 ExecutorEvent 桥接（re-export 自 peri-agent）
//!
//! 实际实现已迁移到 `peri_agent::agent::events_v2_mapper`，因为该 mapper 是
//! 纯函数操作 peri-agent 类型（ExecutorEvent / RenderEvent / StateEvent / ObserveEvent），
//! 多个 crate（peri-acp 主 executor / peri-middlewares SubAgent 转发器）都需要复用。
//! peri-acp 在此 re-export 保持向后兼容，避免下游引用断裂。

pub use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
```

文件名 `mapper_v2` 暗示它与 `mapper.rs`（509 LOC 实体映射逻辑）是同等体量的姊妹模块，实际是 1 行 `pub use`。注释里「保持向后兼容，避免下游引用断裂」是一个**未经验证的假设**——见 §2.3。

### 2.3 re-export 的实际消费者（grep 证据）

执行全仓 grep 查找 `mapper_v2` 模块路径与 `pub use` 导出符号的所有引用：

```
$ grep -rn "render_event_to_executor\|observe_event_to_executor\|state_event_to_executor\|mapper_v2" \
    --include="*.rs" /Users/konghayao/code/ai/perihelion
```

排除 mapper_v2.rs 自身与 peri-agent 的实现后，剩余消费点：

| 消费文件 | 行 | 引用方式 | 是否依赖 mod.rs 的 `pub use` |
|---------|-----|---------|---------------------------|
| `peri-acp/src/event/mod.rs:20` | 20-22 | `pub use mapper_v2::{...}` | —（自身） |
| `peri-acp/src/event/forwarder.rs:23` | 23-25 | `use crate::event::mapper_v2::{...}` | ❌ 否（走模块路径） |
| `peri-agent/src/agent/subagent_event_forwarder.rs:28` | 28 | `use ...events_v2_mapper::{...}`（直接引用 peri-agent 源） | ❌ 否 |

进一步验证 `pub use` 导出的 4 个符号是否被任何外部 crate 通过 `peri_acp::event::xxx` 形式消费：

```
$ grep -rn "peri_acp::event::V2Event\|peri_acp::event::render_event\|peri_acp::event::observe_event\|peri_acp::event::state_event\|use peri_acp::event::\{" \
    --include="*.rs" /Users/konghayao/code/ai/perihelion
（无匹配）
```

**结论**：mod.rs 的 `pub use mapper_v2::{...}` 在整个仓库**零外部消费者**。注释里「避免下游引用断裂」的假设不成立——没有任何下游通过 `peri_acp::event::render_event_to_executor` 这种路径引用它。`forwarder.rs` 是它唯一的实际使用方，且走的是 `crate::event::mapper_v2::` 私有模块路径。

这意味着方向 B（直接删 mapper_v2.rs，把 `forwarder.rs` 的 import 改成直接指向 `peri_agent::agent::events_v2_mapper`）今天就是安全的，不需要等候选 4。

---

## 3. 约束

### 3.1 事件映射的不变量

任何重命名/内联不得破坏以下 5 条事件映射不变量（来自 `mapper.rs` 文件头注释 §Category ①-④ + `forwarder.rs` 文件头注释）：

1. **四类路由互斥**：`MappedEvent` 的 `forward_to_tui` / `hitl_pending` / `observable` 三个 bool 必须按 Category ①-④ 语义设置，`mapper.rs` 是唯一裁决者。
2. **`executor_event_to_acp` 是 DTO 单向门**：`ExecutorEvent → AcpEvent` 仅在 `event_sink.rs:127` 一处消费，把 ExecutorEvent 投递到 `peri/agent_event` 通道给 TUI。反向不存在。
3. **forwarder biased select 顺序不可变**（`forwarder.rs:52-81`）：`render_rx` 必须先于 `state_rx` 被 select，否则跨迭代会产生「新文本在旧工具之前」的 partial 污染。重命名 mapper_v2 不得改动 `forwarder.rs` 的 select 结构。
4. **observe_rx Lagged 容错**：`forwarder.rs:73` 的 `Lagged(n)` 只 warn 不 panic，重命名不得改动该分支。
5. **`router.rs` 的 §5.1 丢弃清单不可变**（`router.rs:108-131`）：15 个 ExecutorEvent 变体必须返回 `None`，重命名 mapper 不影响 router（router 是独立模块）。

### 3.2 ACP 标准复用决议（2026-07-07）

`docs/design/decisions/2026-07-07-acp-reuse-first.md` 规定：标准 ACP `session/update` 优先，自定义事件走 `peri/unstable-event`。`mapper.rs::map_event` 是这条决议的执行者（Category ① 走标准 SessionUpdate，Category ③ 走 TUI-only），其语义不可因重命名而松动。

### 3.3 forwarder 模块的 biased select 顺序

`forwarder.rs:23-25` 的 import 是 mapper_v2 唯一的实际消费点：

```rust
use crate::event::mapper_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};
```

重命名后此 import 路径必须同步更新。若走方向 B（内联删除），此 import 应改为：

```rust
use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};
```

---

## 4. 依赖关系

### 4.1 前置依赖

**无**。本候选与候选 1（visitor）/ 候选 4（dispatch registry）无强耦合——mapper_v2 既不是 visitor 的目标，也不在 dispatch registry 合并清单的硬依赖链上（候选 4 只是把 mapper_v2 列为 deletion test 通过的第 9 项，可以独立处理）。

### 4.2 后置依赖

- **候选 1（visitor 落地）后，mapper_v2 可能整体消失**：若 peri-agent 内部 v2 事件总线被 visitor 模式取代，`render_event_to_executor` 等桥接函数可能被 visitor 的 `visit_render` 方法替代。此时 mapper_v2.rs 与 `events_v2_mapper.rs` 同时废弃。本候选的重命名（方向 A）是过渡期的卫生改进，即使候选 1 最终落地，重命名期间减少的混淆仍是净收益。
- **候选 4（re-export 内联惯例）落地后**：候选 4 在 `04-dispatch-registry-consolidation.md` 的清单里把 `mapper_v2.rs` 列为第 9 项「deletion test 通过」。本候选的方向 B 就是候选 4 对 mapper_v2 的具体执行。两者对 mapper_v2 的处置一致（删），只是本候选聚焦命名维度，候选 4 聚焦 registry 维度。

### 4.3 平行

- **候选 6（trait 可测性）无交互**：mapper_v2 是纯 re-export，不涉及 trait 边界。

---

## 5. 加深后的模块形状

### 方向 A：重命名 mapper_v2.rs → events_v2_bridge.rs（推荐）

**interface 草案**：

```rust
// peri-acp/src/event/events_v2_bridge.rs
//! v2 事件 → v1 ExecutorEvent 桥接（re-export 自 peri-agent）
//!
//! 模块名 `events_v2_bridge` 明确方向：v2 → ExecutorEvent（反向）。
//! 与 `mapper.rs`（ExecutorEvent → ACP，正向）方向相反，命名上区分。
//!
//! 实际实现位于 `peri_agent::agent::events_v2_mapper`，因为该 mapper 是纯函数
//! 操作 peri-agent 类型，多个 crate 复用。本模块仅做 peri-acp 内部 re-export。

pub use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
```

`mod.rs` 同步更新：

```rust
// peri-acp/src/event/mod.rs
pub mod events_v2_bridge;  // 原 mapper_v2
pub mod mapper;            // 不变

// 正向映射（ExecutorEvent → ACP）
pub use mapper::{executor_event_to_acp, map_event, MappedEvent};
// 反向桥接（v2 → ExecutorEvent）——如非必要不对外暴露，见方向 B
pub(crate) use events_v2_bridge::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};
```

注意：`pub use` 收窄为 `pub(crate) use`——因为 §2.3 已证明无外部消费者，收窄可见性是免费的深度加固。

**理由**：

- 文件名 `events_v2_bridge` 直接表达「桥接」语义，方向明确（v2 → ExecutorEvent）。
- 与 `mapper.rs`（正向）形成命名对比：mapper = 正向映射，bridge = 反向桥接。
- 机械重构，零语义变更，`cargo check` 即可验证。
- 同时把 `pub use` 收窄为 `pub(crate) use`，回收不必要的公开面。

### 方向 B：内联删除 mapper_v2.rs（可选，需配合 forwarder.rs 改 import）

**interface 草案**：

```rust
// peri-acp/src/event/mod.rs
// mapper_v2 模块整体移除——无外部消费者，内部唯一消费方 forwarder.rs
// 直接引用 peri-agent 源。
pub mod mapper;
pub use mapper::{executor_event_to_acp, map_event, MappedEvent};
```

```rust
// peri-acp/src/event/forwarder.rs
// 原：use crate::event::mapper_v2::{...};
// 改：直接引用 peri-agent 源
use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};
```

**理由**：

- §2.3 的 grep 证据证明 mapper_v2.rs 是一个**零外部消费者的 re-export**，它的存在只是给 `forwarder.rs` 提供了一层路径缩写。
- 删除后 `forwarder.rs` 多打一次 `peri_agent::agent::events_v2_mapper::` 全路径，复杂度不上升。
- 与候选 4 的 re-export 内联方向一致，可先行落地作为候选 4 的第一个 case。

**风险**：若未来候选 1 落地、`events_v2_mapper` 被废弃，`forwarder.rs` 需要二次修改。但这是所有 v2 桥接代码的共同命运，不是方向 B 独有的代价。

### 方向 C：给 mapper.rs 补文件头方向注释（配合 A）

**interface 草案**：

```rust
// peri-acp/src/event/mapper.rs（仅改文件头注释，不动代码）
//! ── 正向映射：ExecutorEvent → ACP 协议产物 ────────────────────────────
//!
//! 本模块是 ExecutorEvent 的「正向出口」：把 peri-agent 产出的 ExecutorEvent
//! 映射为 ACP 协议的三类产物：
//! - `map_event` → `MappedEvent`（含 `SessionUpdate`，四类路由）
//! - `executor_event_to_acp` → `AcpEvent` DTO（peri/agent_event 通道）
//!
//! 与 `events_v2_bridge.rs`（v2 → ExecutorEvent，反向入口）方向相反。
//! 修改本模块时请同步检查 `mapper_test.rs`（608 LOC）的四类路由覆盖。
```

**理由**：

- `mapper.rs` 当前的文件头注释（§Category ①-④）只描述了路由分类，没有声明自己的**方向**与**对应反向模块**。
- 补一行「与 events_v2_bridge.rs 方向相反」的交叉引用，成本极低，但对新人读代码的导航价值高。
- 与方向 A 完全正交，可叠加。

### 推荐方向

**A + C 组合**。

理由：

1. **A 是主菜**：重命名立即消除「两个 mapper」的命名歧义，是本候选的核心价值。
2. **C 是免费赠品**：补一行注释，零代码变更，零风险，提升 mapper.rs 的可导航性。
3. **B 推迟到 Phase 2**：虽然 §2.3 证明 B 今天就安全，但 B 涉及删除文件 + 改 forwarder import 路径，比 A 的纯重命名多一层语义变更。等候选 4 的 re-export 内联惯例建立后，B 可以作为候选 4 的一个 case 顺势落地，避免本候选承担两件事的 review 负担。
4. **leverage 评估**：A 修改 2 个文件（mod.rs + mapper_v2.rs 重命名），影响 1 个消费点（forwarder.rs 的 use 路径），杠杆比极高。B 同样修改 2 个文件但包含删除操作，杠杆比略低且语义更重。

---

## 6. seam 后面剩什么

### 6.1 mapper.rs（正向映射，不变）

509 LOC，3 个 pub fn（`map_event` / `executor_event_to_acp`）+ 1 个 pub struct（`MappedEvent`）+ 1 个私有 helper（`infer_tool_kind`）。本候选不动其代码，仅方向 C 补文件头注释。

### 6.2 mapper_v2.rs → events_v2_bridge.rs（重命名）或删除（方向 B）

方向 A 后剩 10 行 re-export + 文件头注释，文件名换成 `events_v2_bridge.rs`，`pub use` 收窄为 `pub(crate) use`。

方向 B 后该文件不存在，`mod.rs` 移除 `pub mod mapper_v2;` 行。

### 6.3 调用点 import 路径变化清单

| 调用点 | 当前 import | 方向 A 后 | 方向 B 后 |
|-------|------------|----------|----------|
| `event/mod.rs:10` | `pub mod mapper_v2;` | `pub mod events_v2_bridge;` | （删除该行） |
| `event/mod.rs:20-22` | `pub use mapper_v2::{4 个符号};` | `pub(crate) use events_v2_bridge::{4 个符号};` | （删除该块） |
| `event/forwarder.rs:23-25` | `use crate::event::mapper_v2::{3 个符号};` | `use crate::event::events_v2_bridge::{3 个符号};` | `use peri_agent::agent::events_v2_mapper::{3 个符号};` |
| `mapper_test.rs` | 无引用 mapper_v2 | 不变 | 不变 |
| `router_test.rs` | 无引用 mapper_v2 | 不变 | 不变 |

**关键观察**：方向 A 的调用点变化集中在 3 处，且全部是机械的路径替换。方向 B 的调用点变化也是 3 处，但 `forwarder.rs` 的 import 指向了 peri-agent 而非本 crate 模块——这是一个语义变化（不再经过本 crate 的命名空间），需要在 review 时明确。

---

## 7. 测试面

### 7.1 现有测试全部存活

```
$ wc -l peri-acp/src/event/{mapper_test,router_test}.rs
   608 peri-acp/src/event/mapper_test.rs
   218 peri-acp/src/event/router_test.rs
   826 total
```

| 测试文件 | LOC | 测试对象 | 方向 A 后 | 方向 B 后 |
|---------|-----|---------|----------|----------|
| `mapper_test.rs` | 608 | `map_event` / `executor_event_to_acp` / `MappedEvent` 构造器 | 全部存活（路径未变） | 全部存活 |
| `router_test.rs` | 218 | `route` / `RoutingOutput` | 全部存活 | 全部存活 |
| `events_v2_mapper.rs` 内嵌 tests（peri-agent） | ~400 | `render_event_to_executor` 等 | 全部存活（实现未动） | 全部存活 |

**根因**：方向 A/C 不改任何实现代码；方向 B 只改 `forwarder.rs` 一处 import，且 `forwarder.rs` 本身无单元测试（其行为由 executor 集成测试覆盖）。

### 7.2 可能新增的「方向标注测试」

可选地新增一条编译期断言，确保 `mapper_v2` 的 pub use 不被意外重新提升为 `pub`：

```rust
// peri-acp/src/event/mod.rs（方向 A 后）
// 编译期断言：events_v2_bridge 的 4 个符号不得对外暴露
// 若未来有人误改 pub(crate) 为 pub，该断言仍编译通过，
// 但 events_v2_bridge.rs 文件头注释明确声明了 pub(crate) 意图。
```

实际上这条断言难以用 Rust 表达（`pub` vs `pub(crate)` 是可见性，不是类型属性）。更实际的做法是在 `events_v2_bridge.rs` 文件头注释里写明「本模块仅 pub(crate)，不得对外暴露」，靠 code review 把关。本候选不强制新增测试——重命名是机械重构，测试不变性由现有 826 LOC 覆盖。

---

## 8. 风险与回滚

### 8.1 风险评估

| 风险 | 概率 | 影响 | 缓解 |
|------|-----|-----|-----|
| 重命名后遗漏某处 import 更新 | 低 | 编译错误，`cargo check` 即可捕获 | Phase 1 用 `rustfmt` + `cargo check --workspace` 验证 |
| 外部 fork 通过 `peri_acp::event::mapper_v2` 路径依赖 | 极低 | fork 编译失败 | 本项目无外部 fork 消费者（§2.3 grep 证据）；且这是私有 crate，不发布 crates.io |
| 方向 B 后 `forwarder.rs` import 指向 peri-agent，破坏封装 | 低 | `forwarder.rs` 直接依赖 `peri_agent::agent::events_v2_mapper` 路径，但该路径本身就是 `pub` 的 | peri-agent 已主动暴露该模块，无封装破坏 |
| 候选 1 落地后 mapper_v2 被废弃，本候选重命名变成浪费工作 | 中 | 重命名的工作量（~30 分钟）沉没 | 即使候选 1 落地，过渡期（数月）内重命名减少的混淆仍是净收益；且候选 1 尚未启动 |

### 8.2 回滚

**方向 A 回滚**：

```bash
git revert <commit>
```

重命名是纯机械重构，单个 commit 即可完整回滚。无数据迁移、无配置变更、无协议变更。

**方向 B 回滚**：

```bash
git revert <commit>
# 恢复 mapper_v2.rs 文件 + mod.rs 两行 + forwarder.rs import
```

方向 B 涉及文件删除，但 git revert 能完整恢复。建议方向 B 单独一个 commit，便于独立回滚。

---

## 9. 迁移步骤

### Phase 1：重命名 mapper_v2.rs → events_v2_bridge.rs（方向 A + C，必做）

**改动清单**：

1. `git mv peri-acp/src/event/mapper_v2.rs peri-acp/src/event/events_v2_bridge.rs`
2. 编辑 `events_v2_bridge.rs` 文件头注释，明确「v2 → ExecutorEvent 反向桥接」方向，标注与 `mapper.rs` 方向相反。
3. 编辑 `event/mod.rs:10`：`pub mod mapper_v2;` → `pub mod events_v2_bridge;`
4. 编辑 `event/mod.rs:20-22`：`pub use mapper_v2::{...}` → `pub(crate) use events_v2_bridge::{...}`（收窄可见性）
5. 编辑 `event/forwarder.rs:23-25`：`use crate::event::mapper_v2::{...}` → `use crate::event::events_v2_bridge::{...}`
6. 编辑 `mapper.rs` 文件头注释（方向 C）：补一行「与 events_v2_bridge.rs 方向相反」的交叉引用。

**验证**：

```bash
cargo check --workspace
cargo test -p peri-acp --lib
cargo test -p peri-agent --lib -- events_v2_mapper
```

**提交**：单个 commit，message: `refactor(acp): rename mapper_v2 → events_v2_bridge for direction clarity`。

### Phase 2：内联删除 events_v2_bridge.rs（方向 B，可选，依赖候选 4 惯例）

**前置条件**：候选 4 的 re-export 内联方向已在至少 2 个其它 re-export 文件（如 `event/dto.rs`、`hooks/mod.rs`）上验证过，形成惯例。

**改动清单**：

1. 删除 `peri-acp/src/event/events_v2_bridge.rs`
2. 编辑 `event/mod.rs`：移除 `pub mod events_v2_bridge;` 与 `pub(crate) use events_v2_bridge::{...};` 两行
3. 编辑 `event/forwarder.rs:23-25`：
   ```rust
   // 原：use crate::event::events_v2_bridge::{...};
   use peri_agent::agent::events_v2_mapper::{
       observe_event_to_executor, render_event_to_executor, state_event_to_executor,
   };
   ```

**验证**：

```bash
cargo check --workspace
cargo test -p peri-acp --lib
```

**提交**：单个 commit，message: `refactor(acp): inline events_v2_bridge re-export into forwarder (candidate 4 application)`。

### 何时跳过 Phase 2

若候选 1（visitor）先于 Phase 2 启动，且 candidate 1 会废弃整个 `events_v2_mapper`，则 Phase 2 无意义——直接执行候选 1 的废弃流程。Phase 2 仅在候选 1 长期搁置时作为卫生改进。

---

## 完成判据自检

- [x] 文档行数 300-500（实测约 420 行）
- [x] 至少 3 个 file:line 证据（mod.rs:19-22 / forwarder.rs:23-25 / mapper.rs:339 / router.rs:108-131 / events_v2_mapper.rs:20-231）
- [x] 3 个方向的 Rust 草案（§5.A / §5.B / §5.C）
- [x] 9 节齐全（摘要 / 现状诊断 / 约束 / 依赖 / 模块形状 / seam / 测试 / 风险 / 迁移）
