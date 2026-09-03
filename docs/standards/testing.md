# Perihelion 测试规范

> 状态：现行标准
>
> 本文是测试范围、证据与命令的单一事实源；具体架构测试入口仍以相邻代码和
> `docs/standards/architecture-contracts.md` 的 Verify 项为准。

> 根 workspace 范围以根 `Cargo.toml` 与 `cargo metadata --no-deps` 为准；submodule 与独立项目必须进入各自目录执行其本地命令。

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
- **独立/side project**：不纳入根 workspace 的测试门禁；若目录内存在 manifest 或测试，则按该项目本地命令验证，不能一概标记为“零测试”

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

`peri-agent/src/agent/events_v2_test.rs` 中的回归测试：

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

### TEST-EVIDENCE-001

- **Scope**：测试、lint、build、workflow 与后台验证任务。
- **Rule**：验证结论必须来自最终退出状态和可核对结果。过滤测试须确认实际执行了非零目标用例；不得用未启用 `pipefail` 的 `| tail`、`| grep` 等 pipeline 作为通过证据；后台任务、workflow 和 CI 必须等待终态，并核对阶段结果非空、未截断。预期失败用例按验收目标分类，不按输出中的 `error` 字样机械判失败。
- **Verify**：报告精确命令、exit status、实际执行用例数或结构化 verdict；所有检查标注 passed / skipped / blocked，started 或 completed 通知本身不算通过。

### TEST-HERMETIC-001

- **Scope**：访问 HOME、cache、配置、凭据、时间、网络或进程级全局状态的测试。
- **Rule**：测试必须使用临时 HOME/cache/config 和固定时钟；不得默认读取开发者 `~/.peri`、真实凭据、持久 cache 或外部网络。修改进程级环境变量或全局状态时必须隔离并串行化。时间窗口、TTL、epoch、backoff 与碰撞测试使用可控时钟，不以真实墙钟断言精确次数。
- **Verify**：单独运行与全套运行结果一致；重复或并发运行稳定；测试结束后不修改用户目录或真实配置。

### TEST-LIFECYCLE-001

- **Scope**：持久缓存、动态 discovery、异步通知、重连/恢复、外部进程和 transport 行为。
- **Rule**：集成测试必须跨越功能声称支持的真实生命周期。持久缓存至少覆盖 cold fetch → 写盘 → 进程退出 → 新进程 warm hit → invalidate 后回源；动态通知至少覆盖首发与状态变化后的后续事件；transport 行为使用真实 wire/ordering；外部进程功能必须在声称支持的平台执行 runtime 路径。静态共享代码、单进程 unit test、cross-target compile 或 ignored test 不得替代这些证明。
- **Verify**：断言用户可观察结果及必要的请求数、事件顺序、进程边界或目标平台结果；无法运行的矩阵明确标记 blocked/unsupported。

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

// 中等 mock：手写 trait impl（具体 suggester）
struct MockBashCommandSuggester;
impl ErrorSuggester for MockBashCommandSuggester {
    fn suggest(&self, _ctx: &ErrorContext) -> Option<Suggestion> {
        Some(Suggestion { summary: "来自 MockBashCommandSuggester".into(), details: None })
    }
}
// 注：实际 suggester 共 7 个（BashCommand / GlobPattern / Path / Range / Subagent / Regex / JsonSchema），
// 测试时按需 mock 具体类型即可。

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
- [ ] 新增 `ExecutorEvent`/`ObserveEvent` 变体 → `mapper_test.rs` 增加映射测试 + `variant_coverage_test.rs` 扩展覆盖
- [ ] 新增 v2 事件变体（`RenderEvent`/`StateEvent`/`ObserveEvent`） → `events_v2_test.rs` 与 `events_v2_mapper_test.rs` 的对应覆盖同步更新（如 peri-acp/event 层有对应映射）
- [ ] 新增 Core 工具 → `core_tools_test.rs` 同步
- [ ] 新增中间件 → `before_agent`/`after_agent`/`before_tool`/`after_tool` 关键路径各 ≥1 条
- [ ] 文件系统工具操作 → 各错误路径（not found / ambiguous / permission / not unique）
- [ ] 回归修复 → 带 `/// [回归测试]` 注释 + 历史背景
- [ ] 新增 `SessionUpdate` 变体 → 同步 TUI 侧 `acp_notifier.rs` handler

---

## 八、运行命令

### 8.1 根 workspace

根 workspace 的实际 package 集合以 `cargo metadata --no-deps` 为验证事实源；根 `Cargo.toml` 的 `[workspace].members` 是显式入口，但 Cargo 还可能自动纳入 workspace 根目录下、被成员依赖的 path package。不要只按目录名、members 文本或历史 issue 推断最终集合。

```bash
# 列出当前根 workspace packages
cargo metadata --no-deps --format-version 1

# 根 workspace 全量运行（包含 peri-theme）
cargo test --workspace

# 单 crate
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-middlewares --lib
cargo test -p peri-tui --lib
cargo test -p peri-theme

# 单测过滤
cargo test -p <crate> --lib -- <test_name>

# pre-commit（不含测试）
lefthook run pre-commit    # fmt + check + clippy + typos
```

### 8.2 Peri TUI E2E（分层门禁）

Rust 单测与 E2E 分离：**E2E 命令只在 `e2e/CLAUDE.md` 维护**，不要写「28 个文件逐个手跑」或 `npm run e2e -- --all`。

在 `e2e/` 目录：

```bash
npm run e2e:l0              # PR 冒烟
npm run e2e:l1              # 合并前（panels + tool-cards + smoke）
npm run e2e:release         # 发版全量（含首轮 flake 预算）
npm run e2e:release:strict  # 发版且不容忍首轮 flake
```

单文件调试：`npm run e2e -- --file tests/<path>.test.ts --serial --retry 0`

### 8.3 Submodule 与独立项目

- `.gitmodules` 当前声明 `peri-cool/` 与 `e2e/tui-tester/`；它们不是根 Cargo workspace crate。先初始化 submodule，再进入其目录读取 manifest/README 并运行本地命令。
- `side-projects/` 下项目有各自的 `Cargo.toml` 或 `package.json`，不由 `cargo test --workspace` 覆盖。只在任务涉及对应项目时进入目录，按其 manifest scripts 或 README 执行。
- 仓库顶层出现目录不等于属于根 workspace。`peri-theme/` 虽未写入根 `Cargo.toml` 的显式 members 列表，但当前 `cargo metadata` 将其识别为 workspace package，因此使用 `cargo test -p peri-theme`；分类以命令输出为准。
- `acp-hub/` 与 `agm/` 当前工作树中不存在 manifest，且近 14 天历史显示其代码已删除；不得保留或执行旧测试矩阵命令，也不得从历史 issue 猜测迁移位置。仅在当前代码重新出现 manifest 后，按其实际归属补回命令。

---

## 九、根 Workspace Crate 测试覆盖范围（参考）

测试数量随演进变化，不作为规范基线；以根 `Cargo.toml`、`cargo metadata`、crate 内测试与 CI 为准。下表只列当前根 workspace packages，不包含 submodule 或独立/side projects。

| Crate | 主要测试类型 |
|-------|--------------|
| peri-middlewares | 工具、中间件、MCP、hooks、plugin、subagent |
| peri-acp | 事件映射、session、命令、prompt、协议适配 |
| peri-agent | v2 事件、线程、LLM 适配、中间件链 |
| peri-controller | Langfuse bridge、控制面与跨层转发 |
| peri-runtime | 多 session 编排、事件路由、cancel 转发 |
| peri-resources | 配置、会话存储与外部资源 context |
| peri-model | provider 无关协议、流式适配、传输与重试 |
| peri-tui | kit、配置、同步、CLI 与 ACP client |
| peri-acp-types | DTO、identity、事件与协议 serde roundtrip |
| peri-theme | 主题加载、palette 与 atoms |
| langfuse-client | 客户端、类型、batcher |
| peri-workflow | runner、protocol、registry |
| peri-lsp | 诊断、池、编解码 |
| peri-web-pty | PTY session、WebSocket、HTTP |
