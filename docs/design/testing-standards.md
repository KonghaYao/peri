# Perihelion 测试规范

> 最后更新：2026-07-12
> 全工作区现状：~170 个测试文件，~1500 个测试函数，覆盖 12 个 crate。

---

## 一、测试代码存放位置

| 测试类型 | 存放位置 | 适用条件 |
|----------|----------|----------|
| **单元测试** | 同目录 `_test.rs` 文件（如 `config_test.rs`） | 测试代码 ≥ 30 行 |
| **单元测试** | 同文件末尾 `#[cfg(test)] mod tests { ... }` | 测试代码 < 30 行 |
| **集成测试** | crate 根目录 `tests/` 目录（如 `tests/integration_test.rs`） | 跨模块端到端验证，只能访问 crate 的 `pub` API |
| **测试辅助代码** | 各测试文件内部局部定义 | 不设共享 test_helpers 模块 |

### 文件命名

```
src/foo/mod.rs              # 模块主体
src/foo/mod_test.rs         # 单元测试（≥30行）
src/foo/bar.rs              # 子模块（末尾可含 #[cfg(test)] mod tests）
```

---

## 二、测试优先级分层

### P0：必须测（落入此层漏测 = bug 风险）

| 类别 | 典型示例 | 为何必须测 |
|------|----------|-----------|
| **数据结构序列化** | serde roundtrip、`#[serde(rename)]` 别名、不完全 JSON 反序列化 | 字段错位/丢失直接导致数据损坏 |
| **事件/消息映射** | `ExecutorEvent → SessionUpdate`（`mapper.rs`） | forward_to_tui、source_agent_id 透传错乱→UI 状态残留 |
| **纯逻辑函数** | `StopReason::from_display()`、`infer_tool_kind()`、`derive_key()` | 无 I/O、输入确定→输出唯一，天然可测 |
| **工具系统** | Edit/Write/Read/Bash 文件系统工具 | 每个工具至少 3 条错误路径（not found / ambiguous / permission） |
| **中间件链** | `MiddlewareChain` 顺序、cancel 传播、before/after 钩子 | 中间件顺序错乱 = 安全/功能问题 |
| **CLI/配置解析** | `clap::Parser`、env override 逻辑 | 用户直接暴露的接口 |

### P1：应该测

| 类别 | 典型示例 | 说明 |
|------|----------|------|
| **复杂状态机** | `EditorState` 多行光标、`ThreadStore` SQLite CRUD | 状态转换多→穷举关键路径 |
| **协议编解码** | JSON-RPC codec 帧分割、ACP transport 分片重组 | 边界场景容易出 bug |
| **异步通道** | `EventBus` 三层通道（render/state/observe）、满时丢弃行为 | 需 `#[tokio::test]` |
| **安全敏感** | Crypto encrypt/decrypt、SSRF guard、auth token | 错误密钥、截断数据、边界值 |
| **Prompt 构建** | `build_system_prompt()` sections 拼接、frozen 区域 | 用户暴露的提示词拼错→体验差 |

### P2：可选测

| 类别 | 典型示例 | 说明 |
|------|----------|------|
| 简单 DTO 构造 | `PlanEntry::default()` 所有字段默认值 | 低价值，但 serde 相关仍归 P1 |
| Display/Debug 实现 | `impl Display for Foo` | 仅当被外部消费时测 |
| 常量/枚举完整列表 | `COMPACTABLE_TOOLS = [Bash, Read, ...]` | 新增/删除项时需同步更新测试 |

### 不测

- **TUI 渲染输出**：ratatui-kit 组件的 render body——眼测即可，无视觉回归框架
- **外部 API 调用**：用 mock 替换 trait 接口，不测实际网络
- **纯样板代码**：`fn get_x(&self) -> &X { &self.x }`
- **`side-projects/`**：零测试，随用随弃

---

## 三、测试颗粒度

### 3.1 一条断言法则

每个 `#[test]` / `#[tokio::test]` 验证**一个场景**。不强求一条 assert，但多条 assert 必须验证同一场景的不同侧面。

```rust
// 好：同一场景（Edit 单次替换），三条 assert 验证不同侧面
#[test]
fn test_edit_file_single_replace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello foo world").unwrap();
    let tool = EditFileTool::new(dir.path().to_str().unwrap());
    let result = tool.invoke(json!({"file_path": "f.txt", ...})).await.unwrap();
    assert!(result.contains("Replaced text"), "unexpected: {result}");
    let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(content, "hello bar world");
}
```

```rust
// 好：每个测试一个独立场景
#[test] fn test_edit_file_old_string_not_found() { /* ... */ }
#[test] fn test_edit_file_replace_all()          { /* ... */ }
#[test] fn test_edit_file_ambiguous()            { /* ... */ }
```

```rust
// 反模式：拆分过细，每个 assert 一个测试
#[test] fn test_default_value_x() { assert_eq!(X::default().a, 0); }
#[test] fn test_default_value_y() { assert_eq!(X::default().b, 0); }
```

### 3.2 集成测试颗粒度

验证**一个端到端路径**。如 `Transport → Broker → Approval` 完整流程，或 `Session → Executor → Event → ViewModel` 链路。

### 3.3 回归测试

标注 `/// [回归测试]` 注释，包含**历史背景**（哪个 bug / 哪次修复）。参考 `peri-agent/src/agent/events_v2.rs:1128`：

```rust
/// [回归测试] TurnCompleted 必须在 render_tx 通道中，与同迭代 Render 事件 FIFO。
///
/// 历史背景：TurnCompleted 原在 StateEvent（state_tx 独立通道），biased select!
/// 只保证单次迭代内优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
/// TurnCompleted 被消费，TUI 把 iter2 文本追加到 iter1 partial 上，渲染出
/// "新文本在旧工具之前"的错乱。
#[tokio::test]
async fn test_event_bus_turn_completed_in_render_channel_preserves_cross_iter_order() {
    // ...
}
```

---

## 四、有效测试标准

一个测试满足以下 4 条才算有效：

### 4.1 确定性

无随机数、无时间依赖（mock `Instant::now()` 或参数化）、无外部网络。

### 4.2 覆盖关键路径

正常路径 + **≥1 条错误路径**。不是"跑得通就算"。

### 4.3 断言精确

不用 `assert!(result.is_ok())` 完事。对 error 也要断言 **error 类型/消息内容**。

```rust
// 差：不验证错误类型，无效测试
#[test]
fn test_edit_error() {
    let result = tool.invoke(...).await;
    assert!(result.is_err());  // 什么错都行
}

// 好：精确验证错误内容
#[test]
fn test_edit_file_old_string_not_found() {
    let err = tool.invoke(...).await.unwrap_err();
    assert!(err.to_string().contains("not found"), "should report not found: {err}");
}
```

### 4.4 独立可运行

`cargo test -p <crate> --lib -- <test_name>` 可单独通过，不依赖其他测试的副作用。

```rust
// 差：依赖全局可变状态
static mut COUNTER: i32 = 0;
#[test] fn test_a() { unsafe { COUNTER += 1; } }
#[test] fn test_b() { unsafe { assert_eq!(COUNTER, 1); } }  // 跑单测就挂
```

---

## 五、Mock 与 Fixture 模式

### 5.1 不共享原则

**所有 mock/fixture 在测试文件内部局部定义**。项目不设共享 `test_helpers` 模块，不设 workspace 级测试辅助 crate。

理由：
- 避免 mock 行为隐性耦合（改共享 mock → 多个 crate 测试受影响）
- mock 保持简单（trait 不大→手写成本低）
- 每个测试文件的 mock 可以只实现该文件需要的 trait 方法

### 5.2 标准 mock 模式

```rust
// 简单 stub：make_ 前缀工厂函数
fn make_ids() -> (TurnId, AgentId) {
    (TurnId::new(), AgentId::new())
}

// 中等 mock：手写 trait impl
struct AlwaysSuggest { label: &'static str }
impl ErrorSuggester for AlwaysSuggest {
    fn suggest(&self, _ctx: &ErrorContext) -> Option<Suggestion> {
        Some(Suggestion { summary: format!("来自 {}", self.label), details: None })
    }
}

// 复杂 mock（如 LLM）：手写 trait impl + 返回固定/echo 数据
struct EchoLLM;
#[async_trait::async_trait]
impl ReactLLM for EchoLLM {
    async fn generate_reasoning(&self, ...) -> AgentResult<Reasoning> {
        // ...
    }
}
```

### 5.3 常用测试依赖

| crate | 用途 |
|-------|------|
| `tempfile::TempDir` | 几乎所有 crate——文件系统操作隔离 |
| `serial_test` / `#[serial]` | PTY、全局 atom——需要独占资源的测试 |
| `filetime` | 文件时间戳修改 |
| `mockito` | HTTP mock server（仅 langfuse-client） |
| `temp-env` | 临时环境变量设置 |
| `tokio-test` | tokio 测试辅助 |

### 5.4 禁止项

- 禁止用 `mockall` / `mock!` 宏框架——手写更显式、易调试
- 禁止用 `Mock struct`——CLAUDE.md 明确约定 "Mock `make_` 前缀，不用 Mock struct"
- 禁止在非测试代码中 `#[cfg(feature = "test-utils")]` 导出测试专用 API

---

## 六、测试风格速查

| 项目 | 规范 |
|------|------|
| **命名** | `test_<对象>_<场景>`（如 `test_serde_roundtrip`、`test_llm_call_end_maps_to_enriched_usage_update`） |
| **注释/断言** | 中文 |
| **结构** | **Arrange-Act-Assert 三段，段间无空行** |
| **文件命名** | `_test.rs` 后缀 |
| **异步测试** | `#[tokio::test]` |
| **全局状态测试** | `#[serial]`（依赖全局 atom 的测试）或用 `Mutex<()>` 加锁 |
| **不使用** | 自定义 test harness、`mockall`、`#[cfg(feature = "test-utils")]` |

---

## 七、新增功能 Checklist

- [ ] 新增数据结构（含 serde） → serde roundtrip + 不完全 JSON 反序列化测试
- [ ] 新增 `ExecutorEvent`/`ObserveEvent` 变体 → `mapper_test.rs` 增加映射测试
- [ ] 新增 Core 工具 → `core_tools_test.rs` 同步
- [ ] 新增中间件 → `before_agent`/`after_agent`/`before_tool`/`after_tool` 关键路径各 ≥1 条
- [ ] 文件系统工具操作 → 各错误路径（not found / ambiguous / permission / not unique）
- [ ] 回归修复 → 带 `/// [回归测试]` 注释 + 历史背景
- [ ] 新增 SessionUpdate 变体 → 同步 TUI 侧 `acp_notifier.rs` handler

---

## 八、运行命令

```bash
# 全量运行
cargo test --workspace

# 单 crate
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-middlewares --lib
cargo test -p peri-tui --lib

# 单测过滤
cargo test -p <crate> --lib -- <test_name>

# pre-commit（不含测试）
lefthook run pre-commit    # fmt + check + clippy + typos
```

---

## 九、各 Crate 测试覆盖现状（参考）

| Crate | 测试文件数 | 估算测试数 | 主要测试类型 |
|-------|-----------|-----------|------------|
| peri-middlewares | ~65 | ~550 | 工具、中间件、MCP、hooks、plugin、subagent |
| peri-acp | ~17 | ~230 | 事件映射、session、命令、prompt |
| peri-agent | ~30 | ~174 | 事件、线程、LLM 适配、中间件链 |
| peri-widgets | ~22 | ~170 | widget 渲染、textarea 状态、diff |
| peri-tui | ~12 | ~130 | 配置、同步、CLI、ACP server |
| langfuse-client | ~5 | ~60 | 客户端、类型、batcher |
| agm | ~6 | ~37 | 安装器、存储、过滤 |
| peri-workflow | ~1+ 内嵌 | ~30 | runner、protocol、registry |
| peri-lsp | ~6 | ~30 | 诊断、池、编解码 |
| peri-web-pty | ~5 | ~25 | PTY session、WebSocket、HTTP |
| peri-acp-types | ~1 | ~11 | DTO serde roundtrip |
| peri-theme | ~2 | ~5 | 主题加载器 |
| side-projects/git-stats | 0 | 0 | — |
