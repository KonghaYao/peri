# Perihelion 性能热点分析

> 分析日期: 2026-07-13 | 代码快照: `peri-agent` v2 单路径

---

## 1. 启动性能 — main → agent 构造 → 首次 LLM 调用

### 🔴 `inject_env_from_settings` 双路 JSON 解析（冷路径）

**文件**: `peri-tui/src/main.rs:211-218` + `peri-tui/src/main.rs:225-232`

main() 入口第一条业务逻辑就是**两次完整的 `read_to_string` + `serde_json::from_str`**：
- `inject_env_from_settings()` → 读 `~/.peri/settings.json`
- `inject_env_from_claude_settings()` → 读 `~/.claude/settings.json`

```rust
// main.rs:329-330 — 阻塞 I/O + JSON 解析，在 jemalloc 初始化之后立即执行
inject_env_from_settings();
inject_env_from_claude_settings();
```

每个都执行完整的文件读取+JSON 解析，但只提取 `env` 字段。若 settings.json 很大（大量 plugin/MCP/hooks 配置），这两个调用为 O(file_size) 开销。

**建议**: 用 `serde_json::from_reader` 延迟解析，或仅扫描 JSON token stream 找到 `env` key 即停止。

---

### 🔴 Plugin 加载（磁盘 I/O 密集）

**文件**: `peri-middlewares/src/plugin/loader.rs:492-562`

```rust
// load_enabled_plugins() → load_claude_settings() → read settings.json
// → load_installed_plugins() → 遍历每个插件目录，读 manifest
// → load_manifest() → 读 plugin.json + 多个配置文件
// → extract_hooks() → 读 hooks/hooks.json
// → load_skill_paths() → 扫描 skills 目录树
```

启动链路：
```
main() → App::new() → load_enabled_plugins_aggregated() → {
  load_claude_settings()          // 第 3 次读 settings.json
  load_installed_plugins()         // 第 4 次读 settings.json (读 installedPlugins 字段)
  for each plugin: load_manifest() // N × read_to_string(plugin.json)
  for each plugin: extract_hooks() // N × read_to_string(hooks/hooks.json)
  for each plugin: scan skills dirs // N × walkdir
}
```

**建议**: 
- 合并 settings.json 读取为一次（main() 中解析后传给后续函数）
- 对 plugin manifest 做 mmap + 惰性解析
- 对 skills 目录做缓存（mtime 校验）

---

### 🟡 Skill 扫描（单次启动 ~100-500 次 stat/read）

**文件**: `peri-middlewares/src/skills/loader.rs:93-171`

```rust
pub fn scan_skill_roots(roots: &[SkillRoot]) -> Vec<SkillMetadata> {
    // 对每个 root 递归遍历（深度上限 6，目录上限 1000）
    // 每个目录: canonicalize() + is_dir() + read SKILL.md frontmatter
    // canonicalize() 系统调用开销高（symlink 解析 + 路径规范化）
}
```

`FrozenSessionData::build()` (`peri-acp/src/session/executor.rs:143`) 在 session/new 时调用 `SkillsMiddleware::build_frozen_summary()`，触发全量 skill 扫描。

**建议**: 对 skills 目录做 `HashMap<PathBuf, (SystemTime, Vec<SkillMetadata>)>` 缓存，mtime 不变则跳过扫描。

---

### 🟢 良好的实践: Settings.json 单次加载

**文件**: `peri-acp/src/provider/store.rs:32-38`

```rust
pub fn load() -> Result<PeriConfig> {
    let mut merged = load_from(&config_path())?;   // ~/.peri/settings.json
    if let Some(ws_path) = workspace_config_path() { // {cwd}/.peri/settings.json
        let workspace = load_from(&ws_path)?;
        merged.config.merge_overrides(workspace.config);
    }
    Ok(merged)
}
```

配置在启动时加载一次后存入 `Arc<RwLock<PeriConfig>>`，通过 `ServiceRegistry` 跨组件共享，不会每轮重新解析。这是正确的设计。

---

### 🟢 良好的实践: Frozen Data Flow

**文件**: `peri-acp/src/session/executor.rs:143-188`

CLAUDE.md、skill_summary、system_prompt 在 session/new 时一次性冻结为 `Arc<str>`，不会每轮重建。符合 prompt cache 前缀稳定性要求。

---

## 2. 事件循环热点 — execute_prompt → run_react_loop

### 🔴 每步 ReAct 循环 5 次 clone StageContext

**文件**: `peri-agent/src/agent/stages/mod.rs:516-630`

```rust
// run_react_loop 内每个阶段调用:
let _compact_out = compact::run_compact(CompactInput {
    context: context.clone(),  // ← 第 1 次 clone
    ...
}).await?;
let _receive_out = receive::run_receive(ReceiveInput {
    context: context.clone(),  // ← 第 2 次 clone
}).await?;
let reason_out = reason::run_reason(ReasonInput {
    context: context.clone(),  // ← 第 3 次 clone
    ...
}).await?;
let act_out = act::run_act(ActInput {
    context: context.clone(),  // ← 第 4 次 clone
    ...
}).await?;
let end_out = end::run_end(EndInput {
    context: context.clone(),  // ← 第 5 次 clone
});
```

`StageContext` 包含 ~22 个字段，其中大部分是 `Arc`（clone 便宜），但这不是 zero-cost。5 次 clone × max 500 iterations = 2500 clones per session。

**建议**: 改为 `&StageContext` 引用传递。当前必须 `clone()` 是因为 `StageInput` 要求 owned `StageContext`。可以重构为 `&'a StageContext` 借用。

---

### 🔴 Reason 阶段 — 全量消息 + 工具 clone

**文件**: `peri-agent/src/agent/stages/reason.rs:28-52`

```rust
// 每次 LLM 调用前执行:
let mut messages_snapshot: Vec<BaseMessage> = ctx.visible_messages();  // 全量 clone!
let tools_owned: Vec<Arc<dyn BaseTool>> = {
    let guard = ctx.tools.read();
    guard.values().cloned().collect()  // 全量工具 clone!
};
```

- `visible_messages()` → `transcript.read().visible_messages().into_iter().cloned().collect()` — O(N) 遍历+clone 所有未 excluded 的消息
- `guard.values().cloned()` — 所有工具 Arc clone（~30 个工具 = 轻微）

每 cycle 一次。对话越长越慢。

**建议**: visible_messages() 可以返回 `&[BaseMessage]`（提供切片视图），在传给 LLM 时才做 serialize。当前 clone 是为了避免跨 `.await` 持有 `RwLockReadGuard`，但可以用 defer-drop pattern。

---

### 🔴 Middleware Hook 逐次 clone 全量消息

**文件**: `peri-agent/src/agent/stages/middleware_runner.rs:25-27`

```rust
fn make_context_from_stage(ctx: &StageContext) -> AgentContext<'_> {
    AgentContext::from_stage(ctx)  // ← 每次 hook 都 clone visible_messages()
}
```

**文件**: `peri-agent/src/agent/agent_context.rs:65-72`

```rust
pub fn from_stage(ctx: &'a StageContext) -> Self {
    let messages_cache = ctx.transcript.read()
        .visible_messages()        // Vec<BaseMessage>
        .into_iter().cloned()      // 再次 clone!（visible_messages 本身已 clone）
        .collect();
    ...
}
```

**问题**: `visible_messages()` 已经做了 `collect()`，然后 `AgentContext` 又做了一次 `cloned().collect()` — 即**每条消息被 clone 两次**（一次在 visible_messages 内部，一次在 from_stage）。

每步触发的 hook 次数（从 stages 调用链统计）：
| Hook | 触发条件 | 每步次数 |
|------|---------|---------|
| `before_compact` | compact 阶段 | 1 |
| `after_compact` | compact 阶段 | 1 |
| `before_agent` | act 阶段 | 1 |
| `before_model` | reason 阶段 | 1 |
| `after_model` | reason 阶段 | 1 |
| `before_tools_batch` | dispatch_tools | 1 |
| `after_tools_batch` | dispatch_tools | 1 |
| `after_tool` | 每个工具调用 | N (工具数) |
| `on_error` | 每个错误 | 可变 |

假设 3 个工具调用 + 无错误 → 7 次 `from_stage()` + 7 次全量 clone。假设对话有 100 条消息，每条 ~2KB → 每步 ~1.4MB 内存分配仅用于 middleware hooks。

**建议**: 让 `AgentContext` 复用 `visible_messages()` 返回的 `Vec<BaseMessage>` 而不是重新 clone。

---

### 🟡 TUI 渲染: VIEW_MODELS 每事件 O(N) 全量重建

**文件**: `peri-tui/src/kit/acp_events.rs` (push_view_models)

根据 `peri-tui-render-pipeline-analysis.md:14-20`:
```
每次流式 chunk 到达时:
  1. state.committed.clone()           — im::Vector O(1) clone（共享底层）
  2. items.push_back(vm.clone())       — 每个 current_turn VM O(log n) push
  3. 反向遍历 items 折叠 reasoning     — O(n) 全量扫描
  4. *VIEW_MODELS.state().write() =    — 唤醒所有 5 个订阅者
```

`im::Vector` 的 O(1) clone 是好设计，但**反向遍历折叠 reasoning** 每次都是 O(N) 全量扫描。长对话中（500+ 消息），每次 chunk 到达都触发全量扫描。

---

### 🟢 良好的实践: ScrollThrottle 16ms 节流

**文件**: `peri-tui/src/kit/message_area/` (scroll.rs)

渲染节流确保高频事件不会引发过度重绘。设计合理。

---

## 3. Clone 密集区域

### 🔴 BaseMessage — 大 struct 高频 clone

**文件**: `peri-agent/src/messages/message.rs:59-80`

```rust
pub enum BaseMessage {
    Human { id: MessageId, content: MessageContent },
    Ai { id: MessageId, content: MessageContent, tool_calls: Vec<ToolCallRequest> },
    System { id: MessageId, content: MessageContent },
    Tool { id: MessageId, content: MessageContent, tool_call_id: String },
}
```

`MessageContent` 本身是 `Vec<ContentBlock>`，`ContentBlock` 包含 `ToolResult { content: Vec<ContentBlock> }`（嵌套！）。工具执行结果可能有大量嵌套 block，每次 clone 开销巨大。

**出现频率**:
- 每步 reason 阶段: clone 全部 visible_messages
- 每步 7-9 次 middleware hook: clone 全部 visible_messages
- dispatch_tools: clone reasoning + tool results
- EventBus emit: clone whole reasoning

**建议**: 
- `BaseMessage`/`ContentBlock` 包在 `Arc` 内部（Arc 内用 cheap clone）
- 或者实现 Copy-on-Write 语义（用 `im::Vector` 替换 `Vec<ContentBlock>`）

---

### 🟡 im::Vector<TuiRenderUnit> O(1) clone 抵消大部分开销

**文件**: `peri-tui/src/kit/acp_events.rs:35`

```rust
pub committed: im::Vector<TuiRenderUnit>,
```

TUI 用 `im::Vector`（持久化数据结构）实现 O(1) clone + O(log n) push_back。这避免了每次事件都全量复制历史消息，是正确的选择。

**但**: `TuiRenderUnit` 本身是大 struct，包含 String 和嵌套数据。im::Vector 的 O(1) clone 只共享结构，但 push_back 时新节点仍需 clone element。

---

### 🟢 良好的实践: Feature/Plan 层大量用 Arc

`StageContext` 的 22 字段中 18 个是 `Arc`，包括 `turn`、`transcript`、`llm`、`tools`、`middleware_chain`、`event_bus` 等。clone 成本低。

---

## 4. 数据结构热点 — Cache-Friendly 分析

### 🟡 MessageTranscript: Vec + HashMap 双索引

**文件**: `peri-agent/src/session/transcript.rs:70-80`

```rust
pub struct MessageTranscript {
    entries: Vec<TranscriptEntry>,        // 顺序访问 O(1)
    id_index: HashMap<MessageId, usize>,  // O(1) 查找
    flags: HashMap<MessageId, MessageFlags>,
}
```

- `Vec` 顺序遍历友好，但 `visible_messages()` 需要 `filter(!excluded)` → O(N) 扫描 + 复制
- `id_index` HashMap 在每个 append 时 insert，hash 计算开销为 O(1) 但常数高（UUID hash）

**建议**: 考虑用 `BTreeMap` 替代 `HashMap`（UUID 的 hash 分布均匀性差，BTree 在小尺寸时 cache 更友好）

---

### 🟢 良好的实践: MessageQueue 的数据结构

**文件**: `peri-agent/src/session/queue.rs:118-122`

```rust
pub struct MessageQueue {
    inner: Arc<Mutex<VecDeque<QueuedMessage>>>,
    notify: Arc<tokio::sync::Notify>,
}
```

`VecDeque` 对 FIFO 访问是 cache-friendly 的选择。`Notify` 提供高效的异步唤醒而不需要轮询。

---

### 🟢 良好的实践: FrozenContext 用 Arc<str>

**文件**: `peri-agent/src/session/store.rs:55-67`

```rust
pub struct FrozenContext {
    pub system_prompt: Arc<str>,
    pub claude_md: Arc<str>,
    pub skill_summary: Arc<str>,
    pub date: Arc<str>,
    pub language: Option<Arc<str>>,
}
```

全用 `Arc<str>`（非 `String`），clone 成本 O(1) 仅 refcount 操作。正确。

---

## 5. 协程/消息队列积压

### 🔴 大量 `unbounded_channel` — 无背压机制

**文件** — 搜索 `unbounded_channel` 出现 50+ 处:

| 位置 | 用途 | 风险 |
|------|------|------|
| `peri-tui/src/main.rs:61` | panic 通知 | 低（罕见） |
| `peri-tui/src/kit/entry.rs:128-154` | submit/rewind/ask_user/hitl/thread_load/cancel/bridge | 中（用户交互频率低） |
| `peri-agent/src/session/transcript.rs:148` | 持久化写入 | 🔴 **高** |
| `peri-agent/src/agent/state.rs:106` | 消息投递 | 🟡 中 |
| `peri-acp/src/session/executor.rs:508,539,647` | event pump / bg registry | 🔴 **高** |
| `peri-acp/src/agent/builder.rs:397` | bg_event channel | 🔴 **高** |
| `peri-middlewares/src/cron/mod.rs:72` | cron trigger | 🟡 中 |
| `peri-agent/src/interaction/multiplex.rs:36` | channel 消息 | 🟡 中 |

**核心风险**:
1. **bg_event_tx/rx** (`builder.rs:397`): 后台 subagent 完成事件。若 consumer 处理慢（TUI 渲染卡顿），事件积压不设上限
2. **event pump** (`executor.rs:647`): Agent 事件 flow。高频 LLM streaming = 每秒数十个 chunk 事件，若 TUI bridge 处理不过来会迅速膨胀
3. **持久化 writer** (`transcript.rs:148`): 若 ThreadStore I/O 阻塞（磁盘满/NFS 延迟），persist ops 无限堆积

**建议**: 
- 关键路径（bg_event、event pump）改为 `bounded(N)` channel，设置合理上限（如 1000）
- 对持久化 channel 添加 len() 监控，超过阈值 emit warning

---

### 🟡 MessageQueue 的 Notify 性能

**文件**: `peri-agent/src/session/queue.rs:140-158`

每次 `push()` 和 `push_batch()` 都调用 `notify.notify_one()`，而 `drain_for_end()` 后的 await_wake 也用 `notify.notified().await`。这是对的。但 `push_batch` 中如果有 N 条消息，`extend()` 后只调一次 `notify_one()`（正确）。

---

### 🟢 良好的实践: Cancel token 传播

**文件**: `peri-agent/src/agent/stages/mod.rs:523` + `bulder_v2.rs:162-194`

所有异步任务（cron-bridge、subagent spawn）都通过 `AgentCancellationToken` 链接，支持级联取消。设计合理。

---

## 6. settings.json 是否每轮都重新解析

### 🔴 启动时多次读取 settings.json

启动链路上 settings.json 被读取 **至少 4 次**:

| 顺序 | 位置 | 操作 |
|------|------|------|
| 1 | `main.rs:211` `inject_env_from_settings()` | 完整 read + JSON parse |
| 2 | `main.rs:225` `inject_env_from_claude_settings()` | 完整 read + JSON parse (~/.claude/settings.json) |
| 3 | `plugin/loader.rs:494` `load_enabled_plugins()` | 读 claude settings.json |
| 4 | `plugin/config.rs:462` `load_claude_settings()` | 同上，被多处调用 |
| 5 | `hooks/loader.rs:31` `load_global_settings_hooks()` | 读 `~/.claude/settings.json` hooks 字段 |
| 6 | `skills/mod.rs:28,51` `load_global_skills_dir()` + `load_disable_bundled_skills()` | 读 `~/.peri/settings.json` config.skillsDir |
| 7 | `hooks/loader.rs:132` `load_settings_local_hooks()` | 读 `{cwd}/.claude/settings.local.json` |
| 8 | `store.rs:32` `store::load()` | 主配置加载（全局 + workspace merge） |

其中 (1),(2),(3),(4),(5) 读取的是**同一个文件**（`~/.claude/settings.json`）。

**建议**: 启动时一次性读取 claude settings.json，解析为 `serde_json::Value`，传给所有消费方切片访问（避免重复 I/O + 解析）。

---

### 🟢 良好的实践: 运行时不再解析 settings.json

**文件**: `peri-acp/src/provider/store.rs` + `peri-tui/src/app/service_registry.rs:17`

```rust
pub type SharedPeriConfig = Arc<RwLock<PeriConfig>>;
```

`PeriConfig` 在 `App::new()` 时加载一次，通过 `Arc<RwLock<PeriConfig>>` 共享。session/new 时通过 `PromptExecutionContext.peri_config` 传递，**不会每轮重新读取磁盘**。

**唯一例外**: `PromptFeatures::detect()` 每轮读取 `YOLO_MODE` env var + `is_git_repo` 文件检查（burden 小，可接受）。

---

## 7. 内存泄漏风险 — 无限增长的数据结构

### 🔴 MessageTranscript — entries 永不缩减

**文件**: `peri-agent/src/session/transcript.rs:70-80`

```rust
pub struct MessageTranscript {
    entries: Vec<TranscriptEntry>,  // 只追加，永不删除!
    id_index: HashMap<MessageId, usize>,
    flags: HashMap<MessageId, MessageFlags>,
}
```

Compact 只标 `truncated` / `excluded`，**不删除原始数据**。这意味着:
- 经过 100 轮对话后，transcript 包含所有历史消息（可能 GB 级）
- `visible_messages()` 遍历全量 entries 过滤 excluded（O(N) 随历史增长）
- 每次 middleware hook 都 clone visible_messages()（clone 包含所有 Tool Result 内容）

**建议**: 
- Full Compact 后可选地从 entries 中物理删除 excluded 消息（标记删除 + GC）
- 或者 transcript 只保留可见消息，excluded 消息序列化到磁盘（Lazy loading on demand）

---

### 🔴 TUI VIEW_MODELS — committed 无限累积

**文件**: `peri-tui/src/kit/acp_events.rs:35`

```rust
pub committed: im::Vector<TuiRenderUnit>,  // 永不裁剪!
```

每轮对话的所有消息（文本、工具卡片、思考块）都追加到 committed。长对话（1000+ 条消息）中:
- im::Vector 底层是 RRB-tree，节点共享 → 内存不会因 clone 翻倍
- **但**每个 TuiRenderUnit 包含完整的渲染数据（String、格式化行、wrap_map 缓存）
- 无 eviction 策略

**建议**: 实施窗口限制（如保留最后 10000 条），或与 Compact 联动（excluded 的消息从 committed 移除）。

---

### 🟡 SubAgent Background Registry — task_info 无限堆积

**文件**: `peri-middleware/src/subagent/background_registry.rs` (根据 spec)

Background tasks 完成后状态保留在 registry 中。若不清理，长时间运行的 session 会堆积大量已完成的 task 记录。

---

### 🟡 bg_event_rx — unbounded 管道无上限

**文件**: `peri-acp/src/agent/builder.rs:397-398`

```rust
let (bg_event_tx, bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
```

每个 agent construction 创建一个 bg_event channel。channel 是无界的，如果 TUI consumer 消费速度 < subagent 完成速率，事件会无限堆积。

---

### 🟢 良好的实践: recall_buffer 生命周期管理

**文件**: `peri-agent/src/agent/stages/mod.rs:116`

```rust
pub recall_buffer: Arc<RwLock<Vec<String>>>,
```

recall_buffer 在每个 turn 结束时由 executor drain 清空，不会跨 turn 累积。与 middleware hooks 的 drain 机制配合正确。

---

## 汇总优先级

| 优先级 | 问题 | 文件:行号 |
|--------|------|-----------|
| 🔴 P0 | 启动时 settings.json 被读取 4+ 次 | `main.rs:211-232`, `loader.rs:494`, `config.rs:462` |
| 🔴 P0 | Reason 阶段全量 clone visible_messages + tools | `reason.rs:28-52` |
| 🔴 P0 | Middleware Hook 逐 hook clone 全部消息 (×7-9/step) | `middleware_runner.rs:25-27`, `agent_context.rs:65-72` |
| 🔴 P0 | MessageTranscript entries 永不缩减 | `transcript.rs:70-80` |
| 🔴 P0 | TUI VIEW_MODELS committed 无限累积 | `acp_events.rs:35` |
| 🔴 P0 | 大面积 unbounded_channel 无背压 | 50+ 处 |
| 🟡 P1 | run_react_loop 每步 5 次 clone StageContext | `stages/mod.rs:531-587` |
| 🟡 P1 | Skill 扫描 canonicalize() 系统调用密集 | `skills/loader.rs:193` |
| 🟡 P1 | Plugin 加载链串行磁盘 I/O | `plugin/loader.rs:492-562` |
| 🟡 P1 | TUI push_view_models 每事件 O(N) 全量扫描 | `acp_events.rs` |
| 🟡 P1 | visible_messages() 双次 clone (caller + callee) | `stage_context.rs:201-208` + `agent_context.rs:65-72` |
| 🟢 Good | Settings.json 运行时不再重新解析 | `provider/store.rs` |
| 🟢 Good | Frozen Data Flow 单次构建 | `executor.rs:143-188` |
| 🟢 Good | im::Vector O(1) clone for TUI state | `acp_events.rs:35` |
| 🟢 Good | ScrollThrottle 16ms 节流 | `message_area/scroll.rs` |
| 🟢 Good | Cancel Token 级联传播 | `stages/mod.rs:523` |

---

## Quick Wins（低风险、高收益）

1. **合并 settings.json 读取** — 在 `main()` 中一次性解析，通过参数传递。预计节省 50-100ms 启动时间
2. **AgentContext 复用 visible_messages** — 改为接受 `&[BaseMessage]` 引用而非 clone。消除 ~50% middleware hook overhead
3. **bg_event channel 改为 bounded(1000)** — 防止 subagent 事件积压
4. **Plugin skills roots 添加 mtime 缓存** — 避免每次 startup 全量 walkdir

---

## 深层设计权衡

### visible_messages() Clone 的根因

所有跨 `.await` 边界的生命周期冲突都通过 clone 解决（RwLockReadGuard 不 Send）。这是 Rust async 的经典模式。可行的优化方向：
- **CoW Vector**: 用 `Arc<Vec<BaseMessage>>` 替换每次 clone（但需要处理 mutation 时的 copy-on-write）
- **Slice borrowing with arena**: 用 `typed-arena` 或 `bumpalo` 分配临时消息缓冲区，传递 borrowed slices
- **增量更新**: 维护 `Vec<Range<usize>>` 索引而不是复制消息内容

### unbounded_channel 的工程权衡

当前使用 unbounded_channel 是务实选择（避免死锁）。但需要每个 channel 有独立的上限守护（监控 + 告警），而不是一刀切改 bounded。
