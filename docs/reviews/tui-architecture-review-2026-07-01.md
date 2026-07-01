# peri-tui 架构改进方案

**日期**：2026-07-01
**来源**：三 Agent 并行审核（57 个问题）+ UX 交互调研（10 类问题）
**状态**：提案待审查

---

## 第一部分：架构决策

以下是 11 个核心决策点的拍板，基于成本/收益/风险分析。

| # | 问题 | 决策 | 理由 |
|----|------|------|------|
| 1 | 输入双数据源 | **立即统一**。TextArea 退化为纯渲染 widget，InputState 为唯一模型。删除 `effect_did_mutate_textarea` 标志 | P0 致命，已反复产生 bug |
| 2 | 状态三副本 | **state.view 单源化**。`origin_messages`/`MessageState` 的 pending queues 改为走 `Effect` 路由。`pending_view_rewind_to` → `Event::RewindTo` | 消除一帧延迟和跨边界 flag |
| 3 | v1 渲染删除 | **立即删除** `message_render.rs`，将仍用的工具函数迁入 v2 层。`message_view/` 中未被引用的文件一并删除 | v1 路径已不参与生产渲染 |
| 4 | Effect 压缩 | **输入编辑 14 变体 → SM 内部消费**（不走 Effect）。App 5 个方法变体 → `InvokeApp(AppOp)`。最终 Effect ~12 变体 | 减少循环中无意义的 match 分支 |
| 5 | main_loop 分拆 | **同意分拆**。提取 `dispatch_event`/`execute_effects`/`sync_to_render` 三个子函数。用 struct 封装 snapshot 依赖 | 960 行 → ~50 行主循环 |
| 6 | shortcut 决策 | **分散到各状态模块**。`idle.rs`/`streaming.rs` 分别维护 `owns_shortcut`，删除 `is_sm_handled_shortcut`。fallback 通过"SM 未 consume"判断 | 消除 7 参数布尔函数 |
| 7 | render/ 模块 | **迁移 draw_now 到 render/**。render/ 成为真正的渲染入口，持有 `&mut Terminal` + `&State` | 文档与实现对齐 |
| 8 | 面板 unsafe | **短线方案**：`PanelReadContext` 持有 owned `ServiceRegistrySnapshot`。长线标记 TODO 改为 ACP 查询 | 消除 unsafe，无性能影响 |
| 9 | session 切换 | **统一切换入口** `fn switch_session()`。`ChatSession::drop` 中通过 channel 发送 close 通知 | 防止 ACP server 端 session 泄漏 |
| 10 | markdown 宽度 | **立即传递真实宽度**。v2 渲染函数中的 width 参数接入 `parse_markdown` | 一行改动，高收益 |
| 11 | 集成测试 | **Phase 4 启动**。当前阶段先建立 headless + State 驱动的测试模式，Mock ACP transport 作为 Phase 4 验收标准 | 非阻塞当前重构 |

---

## 第二部分：重构阶段与实施计划

### Phase 0：熵减（预计 1 天）

删除无实际用途的代码/字段/文件，降低后续重构的干扰。

| 步骤 | 操作 | 涉及文件 |
|------|------|----------|
| 0a | 删除 `ViewStore` 结构体，保留 `merge_preserving_local_notes` 等自由函数 | `state_machine/view_store.rs` |
| 0b | 删除 `message_convert.rs` 文件 | `app/message_convert.rs` + `app/mod.rs` 引用 |
| 0c | 删除 `PanelStateStub` 结构体及测试 | `panel/registry.rs` |
| 0d | 删除 `InputState` 中 `at_mention`/`slash_completion` 字段及关联类型 | `state_machine/input/mod.rs` + `transitions/idle.rs` 防御检查 |
| 0e | 简化 `handle_acp_event`：三分支同返回值 → 单行 | `runtime/main_loop.rs:1107-1113` |
| 0f | 删除 `render/throttle.rs`（与 draw_if_needed 功能重复） | `render/throttle.rs` + `render/mod.rs` 引用 |
| 0g | 修复重复 `alloc_collect()` 调用 | `app/thread_ops.rs:217-218` |
| 0h | 删除 `open_thread_browser` 死代码 | `app/thread_ops.rs` |
| 0i | 删除 `theme.rs` 中未使用的 `SUB_AGENT_BG`/`POPUP_BG` | `ui/theme.rs` |
| 0j | 提取 `build_runtime()` 公共函数（消除 6 处重复的 tokio runtime 创建） | `main.rs` |

**验收**：`cargo build -p peri-tui` 无错误，`cargo test -p peri-tui --lib` 全部通过。

---

### Phase 1：状态单源化（预计 3 天）

目标：消除输入双数据源 + 消息三源分裂。

#### 步骤 1.1：InputState 成为唯一输入模型

1. **删除 14 个输入编辑 Effect 变体**：将 `TypeChar`/`DeletePrevChar`/`DeleteNextChar`/`DeletePrevWord`/`DeleteToLineStart`/`SelectAllInput`/`ClearInputBuffer`/`InsertNewline`/`CursorLeft`/`CursorRight`/`CursorUp`/`CursorDown`/`CursorLineStart`/`CursorLineEnd` 从 Effect 枚举移除
2. **SM 转换直接操作 InputState**：`idle.rs`/`streaming.rs` 的键盘处理调用 `InputState` 方法，不 emit Effect
3. **`ReplaceTextarea`/`InsertStr` 合并为 `InputOp::SetText(String)`**：粘贴和外部文本注入走 InputState 原子化更新
4. **删除 `effect_did_mutate_textarea` 标志**：不再需要
5. **2b 同步改为单向**：渲染前 `InputState → to_textarea(TextArea)` 单向同步，删除 `from_textarea` 逆向路径
6. **keyboard fallback 路径不再直接操作 TextArea**：所有 fallback 输入操作改为调用 `InputState.apply(InputOp)` + 返回 Effect 通知 SM

```rust
// 新设计：InputOp 子枚举（在 SM 内部消费）
enum InputOp {
    InsertChar(char),
    DeletePrevChar,
    DeleteNextChar,
    DeletePrevWord,
    DeleteToLineStart,
    SelectAll,
    Clear,
    InsertNewline,
    SetText(String),
    MoveCursor(CursorDirection),
}
```

#### 步骤 1.2：state.view 成为唯一渲染源

1. **删除 `AgentComm.origin_messages: Vec<BaseMessage>`**：改为从 ACP view_mapper 获取 ViewModel
2. **删除 `MessageState` 的 `pending_v2_notes`/`pending_v2_user_bubbles`**：改为 Effect 变体 `PushViewNote(String)` / `PushUserBubble(UserBubbleData)`
3. **`pending_view_rewind_to` → `Event::RewindTo(usize)`**：通过 SM 事件系统处理，消除一帧延迟
4. **`v2_test_views` 测试旁路**：headless 测试改为通过构建 `State { view: Vec<ViewModel> }` 驱动渲染

**验收**：所有输入操作（打字/粘贴/删除/光标/选择）正确渲染；消息流完整；所有 test 通过。

---

### Phase 2：核心循环重构（预计 2 天）

目标：拆分 main_loop 巨型函数 + 简化 Effect 枚举 + 统一快捷键。

#### 步骤 2.1：拆分 main_loop::run

```rust
// 新结构
pub async fn run(rx: EventRx, ctx: &mut ApplyContext<'_>, app: &mut App) -> Result<()> {
    let mut state = State::Idle(IdleState::default());
    let mut frame = LoopFrame::new();

    while let Some(event) = rx.recv().await {
        // 1. 快照（封装为 PreEventSnapshot struct）
        let snap = PreEventSnapshot::capture(&state, app);

        // 2. 事件分发
        let sm_effects = dispatch_sm(&mut state, &event, app);
        let fb_effects = dispatch_fallback(&event, app, &snap);

        // 3. 合并效果（Render 去重，其他追加）
        let effects = merge_effects(sm_effects, fb_effects);

        // 4. 执行效果
        let quit = execute_effects(effects, &mut state, app, ctx).await?;
        if quit { break; }

        // 5. 渲染前同步
        sync_to_render(&state, app, ctx);
    }
    Ok(())
}
```

#### 步骤 2.2：Effect 精简

原始 26 变体 → 目标 ~12 变体：

| 类别 | 新 Effect 变体 | 旧变体映射 |
|------|---------------|-----------|
| 输入（SM 内消费） | *删除，走 InputOp* | TypeChar/DeletePrevChar/... (14个) |
| App 方法 | `InvokeApp(AppOp)` | CycleModel/CycleProvider/CyclePermissionMode/FocusBgBar/ToggleDiff (5个) |
| I/O 副作用 | （保留） | Render/SubmitMessage/PollAgent/Scroll/SendToAcp/CopyToClipboard/PasteText/ShowNotification/UpdateConfig/SwitchSession/OpenPanel/ClosePanel/Quit (13个) |

```rust
// AppOp 子枚举
enum AppOp {
    CycleModel,
    CycleProvider,
    CyclePermissionMode,
    FocusBgBar,
    ToggleDiff,
}
```

#### 步骤 2.3：快捷键分散化

```rust
// idle.rs / streaming.rs 各自实现
impl IdleState {
    fn owns_shortcut(key: &KeyEvent) -> ShortcutClaim {
        match (key.code, key.modifiers) {
            (KeyCode::BackTab, _) => ShortcutClaim::Consumed,
            (KeyCode::Char('t'), ctrl) if ctrl => ShortcutClaim::Consumed,
            // ... 其他快捷键
            _ => ShortcutClaim::NotOurs,
        }
    }
}

// main_loop 中
if State::owns_shortcut(&key) == ShortcutClaim::NotOurs {
    // 交给 keyboard fallback
}
```

**验收**：main_loop.rs ~50 行主循环；所有快捷键行为不变；所有 test 通过。

---

### Phase 3：渲染统一（预计 2 天）

#### 步骤 3.1：删除 v1 渲染代码

1. **删除 `ui/message_render.rs`**（~600 行）
2. **从 `ui/message_view/` 迁出被 v2 引用的函数**（`tool_color`、`ToolCategory` 等）→ 移到 `render/` 或 `ui/` 顶层
3. **删除 `ui/message_view/` 中未被引用的文件**（`build.rs`/`builders.rs`/`aggregate.rs` 中未引用的部分）
4. **保留 v1 类型定义到 `deprecated/` 目录**？**否——直接删除**。v1 `MessageViewModel` 已不在生产渲染路径。

#### 步骤 3.2：render/ 成为真正入口

1. **迁移 `draw_now` 从 `runtime/apply_context.rs` 到 `render/mod.rs`**
2. render 模块持有 `&mut Frame` + `&State` + `&App`
3. 删除 `render/mod.rs` 中的 "FIXME 后续切换" 注释
4. `apply_context.rs` 只保留 `draw_if_needed` 节流逻辑和 `draw_now` 的调用转发

#### 步骤 3.3：Markdown 宽度修复

```rust
// render/view_render.rs: render_v2_vm 中
fn render_user_bubble(data: &UserBubbleView, width: u16) -> Vec<Line<'static>> {
    // 旧：parse_markdown_default(&data.text)  // 固定 80
    // 新：
    let text = if width < 40 { width as usize } else { (width - 6) as usize };
    parse_markdown(&data.text, text)
}
```

#### 步骤 3.4：渲染缓存优化

1. **添加 ViewModel 版本号**：ViewModel 列表获得 version counter，变更时递增
2. **`build_sync_render_cache_v2`**：先检查 version，不变则跳过重建
3. **spinner/todo 行分离**：spinner 行不参与全量缓存重建，作为独立 footer 追加

**验收**：渲染输出与重构前完全一致（差异仅 markdown 宽度）；缓存命中率 >50%（tick 帧大部分不重建）。

---

### Phase 4：解耦 & 加固（预计 3 天）

#### 步骤 4.1：面板 unsafe 修复

```rust
// 旧：PanelReadContext 持有 &ServiceRegistrySnapshot（unsafe 引用延长）
// 新：PanelReadContext 持有 owned ServiceRegistrySnapshot
pub struct PanelReadContext {
    pub snapshot: ServiceRegistrySnapshot,  // owned
    pub vms: Arc<Vec<ViewModel>>,
    pub scroll_offset: u16,
    pub area: Rect,
    pub i18n_lang: &'static str,
}

// TODO(P4): 面板改为通过 ACP 查询获取数据，而非从 App 直接构造 PanelReadContext
```

#### 步骤 4.2：Session 切换统一

```rust
impl App {
    /// 统一的 session 切换入口，确保资源清理
    fn switch_session(&mut self, target: SessionSwitch) -> Result<()> {
        // 1. Cancel 当前 agent
        self.cancel_current_agent();
        // 2. Close ACP session
        self.client.close_session()?;
        // 3. 清理 TUI 侧状态
        self.cleanup_current_session();
        // 4. 打开新 session
        match target {
            SessionSwitch::New => self.client.new_session()?,
            SessionSwitch::Load(id) => self.client.load_session(&id)?,
        }
        Ok(())
    }
}

// ChatSession::drop 中通过内部 channel 通知清理
impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(tx) = &self.close_notifier {
            let _ = tx.send(());
        }
    }
}
```

#### 步骤 4.3：app/ 模块重组

```
app/
├── mod.rs          # 入口 + re-export
├── state/          # 状态管理
│   ├── mod.rs
│   ├── ui_state.rs
│   ├── global_ui_state.rs
│   ├── service_registry.rs
│   └── session_manager.rs
├── events/         # 事件处理
│   ├── mod.rs
│   ├── agent_events_bg.rs
│   ├── agent_events_oauth.rs
│   └── events.rs
├── agent/          # Agent 通信
│   ├── mod.rs
│   ├── comm.rs
│   ├── submit.rs
│   ├── compact.rs
│   └── render.rs
├── ui/             # UI 组件
│   ├── mod.rs
│   ├── interaction.rs
│   ├── hitl.rs
│   ├── ask_user.rs
│   ├── rewind.rs
│   ├── oauth.rs
│   ├── hints.rs
│   └── at_mention/
└── service/        # 基础设施
    ├── mod.rs
    ├── command_system.rs
    ├── chat_session.rs
    ├── history.rs
    ├── cron.rs
    └── provider.rs
```

#### 步骤 4.4：弹窗解耦

```rust
trait InteractionPopup {
    /// 弹窗自己声明期望高度（无需布局代码知道实现细节）
    fn desired_height(&self, available_height: u16) -> u16;
    /// 渲染弹窗（传入已计算好的 area）
    fn render(&self, f: &mut Frame, area: Rect);
}

// status_bar 通过 trait 获取快捷键提示
fn shortcut_hints(&self) -> Vec<(&'static str, &'static str)>;
```

**验收**：面板/弹窗的行为与重构前完全一致；session 切换无资源泄漏；模块编译边界清晰。

---

## 第三部分：UX 交互修复

基于调研发现的 10 类问题，以下修复在 Phase 1-3 中穿插执行。

### 光标相关

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| C1 光标残影修复 | **P1** | Phase 1 | 统一 TextArea→InputState 单向同步后，`cursor_at_end` 残影自动消失（当前原因：TextArea 和 InputState 光标不同步） |
| C2 IME 候选窗 macOS/Linux | P3 | Phase 4 | 调研能否将 Windows 的 IME 坐标计算移植到 macOS/Linux 终端 |
| C3 FieldTextarea 透明背景 | P3 | Phase 3 | POPUP_BG 硬编码移除后，确认 form overlay 穿透问题是否已修复 |

### 快捷键相关

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| K1 移除 `is_sm_handled_shortcut` | **P1** | Phase 2 | 随 shortcut 分散化方案一起解决，消除 7 参数布尔函数 |
| K2 Ctrl+U 在输入框非空时不滚动 | P3 | Phase 1 | 统一行为：输入框非空时 Ctrl+U 删除内容 + 滚动到顶部（分离两次 Effect） |
| K3 macOS Option 键组合 | P3 | Phase 2 | Unicode 字符匹配（`\u{b5}`→CycleModel）保持，但加注释说明 Option 键映射 |

### 输入框相关

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| I1 预测输入（prediction）弃用清理 | **P1** | Phase 1 | 删除 `InputState.prediction` 字段（恒为 None），清理 `transitions/idle.rs:112-119` 防御代码 |
| I2 @mention 目录缓存刷新 | P3 | Phase 4 | `AtMentionState::detect` 时检查 cwd 变化，`dir_cache` 在 cwd 变更时失效（当前只在切换 session 时刷新） |
| I3 输入框历史导航 | P3 | Phase 2 | Up/Down 在空输入框时浏览器历史（`event/keyboard/normal_keys.rs:83-87` 已支持），但需确认与 @mention/hint 的优先级 |

### 焦点管理

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| F1 focused_only 模式 Ctrl+C 中断 | **P2** | Phase 2 | 当 `focused_instance_id` 非None 时，Ctrl+C 应退出 focused_only 模式并中断 agent（当前只 Esc 退出） |
| F2 焦点环视觉指示 | P3 | Phase 4 | bg_bar 焦点切换时在状态栏增加视觉提示（当前无可感指示） |

### 滚动相关

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| S1 滚动步长可配置 | P3 | Phase 4 | 步长固定 3 行，可改为配置项 |
| S2 Ctrl+D 与 Ctrl+U 对称 | P3 | Phase 1 | 随 K2 一起修 |

### 缺失功能

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| M1 MCP 面板 6 个 SendToAcp TODO | P3 | Phase 4 | reconnect/delete/reauthenticate/clear_auth/reconnect/set_disabled 接线 |
| M2 Memory 面板 Enter 编辑 | P3 | Phase 4 | 当前只能导航，不能编辑 |

---

## 第四部分：迁移安全

### 回退策略

每个 Phase 在独立 branch 上开发，Phase 完成并通过 full test suite 后合并。如果某个 Phase 引发不可预见的回归：

1. **Phase 0**：死代码删除，如有引用遗漏 → 补回引用即可
2. **Phase 1**：状态单源化是最危险的重构 → 先做输入模型的单元测试，确认 `InputOp → InputState` 行为与旧路径完全一致后再切
3. **Phase 2**：main_loop 分拆 → 提取子函数时保留旧函数，通过 feature flag 切换
4. **Phase 3**：v1 渲染删除 → 先确认 v2 渲染覆盖所有 v1 场景（对比 headless 快照测试）
5. **Phase 4**：解耦改动独立，出问题可单独回退

### 测试策略

| Phase | 测试方法 |
|-------|---------|
| 0 | `cargo build` + `cargo test --lib` |
| 1 | `InputState::apply(InputOp)` 单元测试（覆盖 CJK/粘贴/删除/光标）+ `from_textarea`/`to_textarea` 往返测试 |
| 2 | `ShortcutClaim` 决策表测试（覆盖所有已知冲突）+ `main_loop` headless 集成测试 |
| 3 | headless 渲染快照对比（重构前后逐像素对比） |
| 4 | session 切换集成测试（mock ACP transport）+ 面板功能测试 |

### 已知风险

1. **Phase 1 输入统一**：keyboard fallback 路径中大量 `textarea.insert_str()` 等直接操作 → 需要全部改为 `InputState.apply()` 调用，工作量大
2. **Phase 3 v1 删除**：`message_render.rs` 的删除可能影响 headless 测试 → 替换为 v2 渲染器
3. **worktree theme-system 分支**：Phase 4 的模块重组可能与此分支冲突 → 需合并后再重组或先沟通

---

## 第五部分：审查反馈与修正

经可行性/完整性/安全性三角度审查，以下对原方案进行修正。

---

### 阻塞级修正（必须采纳）

#### 修正 1：`origin_messages` 不删除，改为降级角色

**原方案**：Phase 1 删除 `origin_messages`，改为从 ACP view_mapper 获取。

**审查发现**：rewind 功能深度依赖 `origin_messages`——遍历 Human 消息构建回退节点、按 `message_id` 查找全文、extract_file_changes、truncate 截断。ViewModel 不包含这些元数据。

**修正方案**：
- `origin_messages` **保留**但退化为 rewind-only 数据源，不参与渲染
- Phase 1 只删除 `origin_messages` 的渲染相关引用（compact 替换、TurnCommitted replace 等），保留 rewind 读写
- 在 Phase 4 引入 `RewindDataSource` trait，通过 ACP server history API 查询 rewind 数据后，再彻底删除

**影响**：Phase 1 删除 `origin_messages` 步骤改为"降级"，Phase 4 增加"RewindDataSource 引入"步骤。

#### 修正 2：Session 切换增加超时和失败恢复

**原方案**：Phase 4 的 `switch_session()` 无超时，`close_session`/`new_session` 可能阻塞主线程。

**审查发现**：`open_thread()` 使用 `block_in_place + block_on`，ACP 无响应时整个 TUI 卡死。

**修正方案**：
```rust
fn switch_session(&mut self, target: SessionSwitch) -> Result<()> {
    self.cancel_current_agent();
    // 先创建新 session（失败可恢复旧 session）
    match target {
        SessionSwitch::New => self.client.new_session_with_timeout(Duration::from_secs(5))?,
        SessionSwitch::Load(id) => self.client.load_session_with_timeout(&id, Duration::from_secs(5))?,
    }
    // 再 close 旧 session（失败不阻塞）
    let _ = self.client.close_session();
    self.cleanup_current_session();
    Ok(())
}
```
- 所有 ACP 调用加 `tokio::time::timeout(5s)`
- 超时时显示 `PushSystemNote("Session switch timed out")` 并保持 Idle 状态
- 顺序改为 new → close（new 失败可恢复，close 失败不致命）

**影响**：Phase 4 步骤 4.2 的伪代码更新。

#### 修正 3：往返测试增加边界覆盖

**原方案**：往返测试仅覆盖 CJK 基本路径。

**审查发现**：缺少空 buffer 光标、col_byte OOB、混合字符集（ASCII+CJK+Emoji）、超大 buffer 测试。

**修正方案**：Phase 1 测试计划增加：
```rust
test_to_textarea_empty_buffer_cursor()     // 空 buffer 后光标在 (0,0)
test_to_textarea_col_byte_oob_clamps()     // col_byte 越界 clamp 保护
test_roundtrip_mixed_ascii_cjk_emoji()     // 多字节混合往返
test_to_textarea_large_buffer()            // 10k 行性能 + 正确性
```

**影响**：Phase 1 测试策略表增加 4 项。

---

### 高风险修正

#### 修正 4：`POPUP_BG` 不可删除（仍被 field_textarea 使用）

**原方案**：Phase 0 步骤 0i 删除 `POPUP_BG`。

**审查发现**：`app/field_textarea.rs:24` 中 `bg(theme::POPUP_BG)` 用于 form overlay 穿透修复。

**修正方案**：
- `SUB_AGENT_BG` → 确认未使用，删除（安全）
- `POPUP_BG` → **保留**，在 Phase 3 与 C3 修复（form overlay 穿透）一起评估是否可用替代方案

**影响**：Phase 0 步骤 0i 拆为两步，`POPUP_BG` 延迟到 Phase 3。

#### 修正 5：`ShortcutClaim` 改为三态枚举

**原方案**：二元 `Consumed / NotOurs`。

**审查发现**：无法表达 Modal 的 "Ctrl+C→fallback，其他→SM" 这种细粒度逻辑。

**修正方案**：
```rust
enum ShortcutClaim {
    SMOwns,       // SM 独占
    FallbackOwns, // fallback 独占
    Defer,        // SM 处理，fallback 兜底
}
```
- Idle/Streaming 状态的 `owns_shortcut` 返回 `SMOwns/Defer`
- Modal 和 popup 的 bypass 逻辑保持在 main_loop 中（不在 owns_shortcut 内处理）

**影响**：Phase 2 步骤 2.3 的枚举定义更新。

#### 修正 6：废弃 `InteractionPopup` trait，复用现有 `Handler` trait

**原方案**：新增 `InteractionPopup` trait 解决弹窗解耦。

**审查发现**：与现有 `state.rs:51-61` 的 `Handler` trait（已有 4 个实现：Hitl/AskUser/Rewind/OAuth）功能重叠。引入新 trait 会导致 modal.rs 渲染路径出现两套 dispatch。

**修正方案**：
- **不引入** `InteractionPopup` trait
- 在现有 `Handler` trait 上增加 `desired_height() -> u16` 方法
- `active_panel_height` 代码移到各 Handler 实现中

**影响**：Phase 4 步骤 4.4 的 trait 定义替换为 `Handler` 扩展。

#### 修正 7：Phase 1 拆为两个子阶段

**原方案**：Phase 1 一步完成输入统一 + 状态单源化。

**审查发现**：输入统一需要重写整个 `event::keyboard` 模块（~400 行），工作量等于 "B3 MigrateInput"。

**修正方案**：
- **Phase 1a**：InputState 成为唯一逻辑模型（InputOp 子枚举、SM 内部消费输入编辑）
- **Phase 1b**：keyboard fallback 迁移到 InputOp（替换所有 `textarea.` 直接操作）
- **整体仍叫 Phase 1**，但内部拆为两步，1a 完成后先验证再进 1b

**影响**：Phase 1 工时从 3d 调整为 4d（1a 2d + 1b 2d）。

#### 修正 8：保持 `pending_v2_notes` 队列模式

**原方案**：`pending_v2_notes`/`pending_v2_user_bubbles` 改为 Effect 变体。

**审查发现**：~8 处 `push_system_note` 调用点都需要增加 Effect 返回路径，改动大且收益低。队列模式是安全的桥接模式。

**修正方案**：
- **保留** `pending_v2_notes`/`pending_v2_user_bubbles` 队列
- drain 点从 main_loop 移到 SM 的 `Event::Tick` 处理器内部（消除一帧延迟）
- 其他状态单源化步骤不变

**影响**：Phase 1 步骤 1.2 的"改为 Effect"改为"保留队列，drain 点内迁"。

#### 修正 9：增加 UX 四项修复

**原方案**：UX 修复清单覆盖 10 类问题，但缺少以下 4 项。

| 修复项 | 优先级 | 阶段 | 描述 |
|--------|--------|------|------|
| U1 输入框超视区自动滚动 | **P2** | Phase 1 | 输入多行超过 textarea 高度时自动滚屏至光标可见 |
| U2 长消息行内折叠 | **P2** | Phase 3 | ToolCard/Diff 超长时提供折叠/展开控制 |
| U3 流式输出中途滚轮行为规范 | **P2** | Phase 2 | 用户手动滚动查看历史时，新 chunk 到达不强制跳回底部（需用户主动滚回底部才恢复 auto-follow） |
| U4 多行输入换行符可见性 | P3 | Phase 4 | Shift+Enter 插入的换行符在 textarea 中的视觉提示 |

**影响**：第三部分 UX 修复清单增加 4 行，Phase 工作量微调。

#### 修正 10：`at_mention`/`slash_completion` 删除需同步 `idle.rs`

**原方案**：Phase 0 步骤 0d 删除 InputState 中的字段。

**审查发现**：`idle.rs` 有三处引用（`FileSuggestions` 处理、Enter 防御检查、Esc 清除），必须同步删除。

**修正方案**：步骤 0d 说明增加"同步删除 `idle.rs` 中三处关联代码"。

**影响**：Phase 0 步骤 0d 的描述更新。

#### 修正 11：Panel unsafe 修复覆盖两处构造点

**原方案**：Phase 4 修复 `PanelReadContext` 持有 owned snapshot。

**审查发现**：`build_panel_read_context` 有两处（`apply_context.rs:424` + `main_loop.rs:1252`），方案只提及一处。

**修正方案**：Phase 4 步骤 4.1 明确标注两处构造点一起修复。

**影响**：Phase 4 步骤 4.1 的描述更新。

---

### 中风险修正

#### 修正 12：ViewStore 删除的完整方案

**原方案**：Phase 0 步骤 0a "删除 ViewStore 结构体，保留自由函数"。

**审查发现**：`for_render()` 是方法而非自由函数，`merge_preserving_local_notes` 被 `state.rs` 的 `view_models()` 调用。删除后需要重构调用点。

**修正方案**：
- 将 `merge_preserving_local_notes` 提升为自由函数 `pub(crate) fn merge_preserving_local_notes(...)`
- 将 `for_render` 转化为 `pub(crate) fn view_for_render(...)` 自由函数
- 在 `state.rs` 的 `view_models()` 中直接调用自由函数
- 删除 `ViewStore` 结构体

**影响**：Phase 0 步骤 0a 的实施细节更新。

#### 修正 13：Phase 3 渲染验证方案

**原方案**：headless 渲染快照对比（重构前后逐像素对比）。

**审查发现**：字符串包含匹配（`snapshot.join("\n").contains("str")`）不是像素对比；v2_test_views 路径不是生产路径；无法枚举所有渲染场景。

**修正方案**：
1. 建立 `TestBackend::buffer()` 逐像素断言（替代 `contains` 字符串匹配）
2. 将 headless 测试从 `seed_v2_*` 改为从 `State { view }` 构造
3. Phase 3 删除 v1 前，先运行 v2 渲染器覆盖所有 v1 `render_view_model` 测试场景
4. `message_render.rs` 中 `render_view_model` 保留为 `#[cfg(test)]` 辅助函数，待 v2 渲染测试迁移完成后删除

**影响**：Phase 3 验收标准和迁移安全策略更新。

#### 修正 14：draw_now snapshot 一致性

**原方案**：draw_now 在 `terminal.draw` 前分别取 `state.view_models()` 和 `current_turn.view_models()`。

**审查发现**：两个快照之间存在时间窗口，但单线程事件循环确保当前不构成 bug。结构上不优雅。

**修正方案**：
- 在 `terminal.draw()` 闭包内部通过 `&State` 读取，避免外部快照不一致
- 或：一次性打包 `struct RenderSnapshot { view, turn_vms, panel_ctx }`

**影响**：Phase 3 渲染统一时顺带优化。

#### 修正 15：max_length 边界保护

**原方案**：未提及输入长度限制。

**审查发现**：粘贴大量文本时 `to_textarea` 的逐行清理 O(n²) 性能差。

**修正方案**：Phase 1 增加 `MAX_TEXTAREA_BYTES = 100KB`，在 `InputState::insert_str` 中截断。

**影响**：Phase 1 输入模型增加一行边界保护。

#### 修正 16：`AppOp` 子枚举的适用范围收窄

**原方案**：CycleModel/CycleProvider/CyclePermissionMode/FocusBgBar/ToggleDiff 五个变体合并为 `InvokeApp(AppOp)`。

**审查发现**：CyclePermissionMode 不保存配置也不触发 ACP，与 CycleModel/CycleProvider 不同质。强行合并会在主循环产生大型内部 match。

**修正方案**：`AppOp` 只覆盖共享"修改配置+保存+ACP 同步"模式的变体（CycleModel、CycleProvider）。其余三个保持独立 Effect 变体。最终 Effect ~13 变体。

**影响**：Phase 2 步骤 2.2 的 Effect 精简方案更新。

---

### 修正后汇总

| 维度 | Phase 0 | Phase 1a | Phase 1b | Phase 2 | Phase 3 | Phase 4 | 合计 |
|------|---------|----------|----------|---------|---------|---------|------|
| 删除文件 | ~7 | ~1 | ~2 | ~1 | ~2 | 0 | ~13 |
| 修改文件 | ~11 | ~10 | ~8 | ~5 | ~8 | ~22 | ~64 |
| UX 修复 | 0 | 2 | 1 | 4 | 2 | 10 | 19 |
| 预计工时 | 1d | 2d | 2d | 2d | 2d | 3d | **12d** |
| 风险 | 低 | 中 | **高** | 中 | 中 | 低 | — |

### 审查结论

方案总体方向正确。16 项修正均为**实施层面调整**，未改变核心架构方向（状态单源化、TextArea 退化为渲染 widget、Effect 精简、v1 删除）。最关键的调整是：`origin_messages` 改为降级而非删除（rewind 功能安全）、Session 切换增加超时和失败恢复、Phase 1 内部拆两阶段降低风险。
