# CLI 全局参数：指定 sqlite 数据库路径与全局配置文件路径

**状态**：✅ 已实施（2026-08-07，devflow: max）
**创建日期**：2026-08-06
**关联**：用户决策 2026-08-06（参数范围=CLI 全局；不支持 settings.json 内 db_path 字段；先出方案不实施）+ 2026-08-07 实施 gate 决策（见文末"实施记录"）

## Problem Statement

两处路径目前硬编码，无法重定向：

1. **sqlite 会话数据库**：`peri-resources/src/context.rs` `Resources::open()` 固定 `~/.peri/threads/threads.db`（失败 fallback 临时目录）。调用点 3 处：TUI（`peri-tui/src/app/mod.rs:78`）、print（`peri-tui/src/cli_print.rs:126`）、`peri acp` stdio（`peri-acp/src/host/stdio/init.rs:118` 经 `peri_agent::resources::open_thread_store()`）。底层 `SqliteThreadStore::new(path)` 已支持任意路径，缺的只是装配面入口。
2. **全局配置文件**：`peri-acp/src/provider/store.rs:8` `config_path()` 固定 `~/.peri/settings.json`；`load()`（全局 + `./.peri/settings.json` 工作区 merge）与 `save()`（原子写回全局）都基于该路径。

**约束**：全局配置文件自身的位置不能写进配置文件（鸡生蛋），只能由 CLI 参数/环境变量指定。已确认一期**不**在 `AppConfig` 中增加 db_path 字段。

## Solution

优先级链：**CLI 全局参数 > 默认值**（本期不做环境变量通道，`PERI_*` 环境变量可作二期）。

### 1. CLI 全局参数（`peri-tui/src/main.rs` `Cli` 结构）

放在 `Cli` 顶层而非 `Acp` 子命令内，TUI / `-p` print / `peri acp` 三路径统一生效。alias 风格对齐现有 `--session-id`/`sessionId`：

```rust
/// 全局配置文件路径（默认 ~/.peri/settings.json）
#[arg(long = "config-file", visible_alias = "configFile")]
config_file: Option<PathBuf>,
/// SQLite 会话数据库路径（默认 ~/.peri/threads/threads.db）
#[arg(long = "db-path", visible_alias = "dbPath")]
db_path: Option<PathBuf>,
```

注意与现有 `--settings` 区分：`--settings` 只注入 env 且接受 JSON 字符串（`inject_settings_override`，main.rs:323），语义不同，不复用。

### 2. 全局配置路径重定向（`peri-acp/src/provider/store.rs`）

`config_path()` 为无参签名、被 TUI/stdio/print 多处复用，改为进程级重定向最贴合现状：

- 新增 `OnceLock<PathBuf>` 保存重定向路径；暴露 `set_global_config_path(Option<PathBuf>)`，由部署装配点（main.rs）在启动早期调用一次。
- `config_path()`：重定向已设置 → 返回该路径；否则原默认值。
- **`save()` 必须写回重定向后的路径**（同样基于 `config_path()`）——否则 UI 修改配置写入 `~/.peri/settings.json`、下次读取的是自定义文件，配置静默丢失。这是本方案最关键的语义点。
- `load()` 不变（基于 `config_path()` + 工作区 merge），工作区 `./.peri/settings.json` 覆盖语义不受影响。
- `workspace_config_path()` 不变（始终相对 cwd）。

### 3. db 路径（`peri-resources/src/context.rs` + 装配链）

- `Resources::open()` 保留默认行为（兼容既有调用与 fallback 语义）。
- 新增 `open_with(db_path: Option<PathBuf>)`：`Some` → 直接 `SqliteThreadStore::new(path)`；`None` → 原逻辑（默认路径 + fallback 临时目录）。
- `peri-agent/src/resources.rs`：`open_thread_store()` 旁新增 `open_thread_store_with(db_path: Option<PathBuf>)` 转发。
- `peri-acp/src/host/stdio/init.rs`：`StdioAssemblyInput` 增加 `db_path: Option<PathBuf>`（可选字段，不破坏现有装配），init 内透传给 `open_thread_store_with`。
- `peri-tui/src/main.rs`：`TuiOptions` 增加 `config_file` / `db_path` 字段；print 分支在 `run_print` 参数列表追加两个参数；`Acp` 分支构造 `StdioAssemblyInput` 时透传。

### 4. fallback 语义决策（待确认项）

显式指定 db 路径时打开失败应**直接报错**，不再 fallback 临时目录——否则用户以为数据在指定位置、实际落在 `/tmp`，造成数据"丢失"假象。默认路径失败 fallback 临时目录的既有行为保留。

## Commits（实施时拆分）

- **C1** CLI 参数解析 + `TuiOptions`/`run_print`/`StdioAssemblyInput` 透传（main.rs、cli_print.rs、stdio/init.rs）；参数解析测试。
- **C2** 配置路径重定向：`set_global_config_path` + `config_path()`/`save()` 改造（store.rs）；测试覆盖重定向后 load/save 一致性、工作区 merge 仍生效。
- **C3** db 路径：`Resources::open_with` + `open_thread_store_with` + 三装配点接入（context.rs、resources.rs、app/mod.rs、cli_print.rs、stdio/init.rs）；测试覆盖指定路径建库、显式路径失败报错、默认行为零变化。
- **C4** 文档路由检查（DOC-UPDATE-001）：CLI 帮助文本/README 若有参数清单则更新。

## 测试计划

- `peri-acp/src/provider/store_test.rs`：重定向后 `config_path()`/`save()` 写回正确路径；重定向 + 工作区配置 merge 顺序不变。
- `peri-resources` sqlite 侧：`open_with(Some(path))` 创建/打开指定库；指定路径不可写时返回错误（不 fallback）。
- `peri-tui/src/cli_args_test.rs` / `cli_integration_test.rs`：`--config-file` / `--db-path` 解析与透传。
- stdio 装配：`StdioAssemblyInput` 新字段默认 None 时行为零变化。

## 实施记录（2026-08-07，devflow: max）

### 实施顺序与原因

实际按依赖驱动顺序 **C2 → C3 → C1 → C4**（C1 的透传依赖 C2 的 `set_global_config_path`/`config_path` 与 C3 的 `open_with`/`open_thread_store_with` API；C2/C3 独立可测，先落地可尽早暴露全局态隔离与 sqlite 错误语义问题）。C1 内"字段 + prescan + 接线"同片落地（拆开出现 never-read 中间态触发 clippy -D warnings）。

### Gate 决策（用户确认）

1. **env 注入 = Option A（两阶段预扫描）**：main() 在 env 注入序列前用 `pre_scan_config_file` 轻量扫描 argv（支持 `--config-file path` / `--config-file=path` / `--configFile`；遇 `--` 停止；**缺值或后跟 option-like token 时立即终止扫描并 fail-open 返回 None**，交给 clap 报错）；`set_global_config_path(结果)` 后执行现有注入序列（仍在 parse 前，`peri web` 的 clap env=HOST/PORT 行为不变）；`inject_env_from_settings` 改读 `config_path()`；parse 后以 `cli.config_file` 幂等重设。
2. **TUI 失败退出码 = exit 1**：`--db-path` 显式指定时，`run_tui` 任一运行期错误均传播并使进程 exit 1（实现措辞比原决策"仅 db 打开失败"更宽——reviewer 判定接受：方向与 gate 意图一致、影响面窄、anyhow 无类型区分无法精确限定）；print/acp 路径经 `?` 传播 exit 1。
3. **`--settings` 并存 = settings 全权替换 config**：print 模式沿用现有 `load_from(settings)` 完全替代语义，`--config-file` 仅负责 env 注入与 save 写回目标；TUI 与 print 对该组合解释差异已在 README 标注。
4. **相对路径 = set 时 absolutize**：`set_global_config_path(Some(p))` 内相对路径按启动时 cwd `join` 绝对化（消除裸文件名保存时 `create_dir_all("")` 报错与 `peri acp --cwd` 基准混淆）。

### 已知限制（本期待标注，二期候选）

- `peri sync`（sync/writer.rs、scanner.rs）与 middlewares 侧 skillsDir/disableBundledSkills/MCP 全局配置仍读写默认 `~/.peri/settings.json`，**不跟随 `--config-file` 重定向**（sync 为任务范围外；middlewares 受依赖方向硬约束——peri-acp → peri-middlewares 无法引用 provider::config_path，二期可下沉共享 crate 或参数化注入）。MCP 面板只读不写，无写读错位丢数据风险。
- `--db-path` 指定后 WAL 伴生文件（`-wal`/`-shm`）落在同目录；父目录不存在时自动创建（`SqliteThreadStore::new` 的 `create_dir_all` 语义，属"打开（或创建）"设计），真实报错路径为父目录不可创建（实测 exit 1 + 错误含路径）。
- print/acp 路径配置加载错误被 `.unwrap_or_default()` 吞掉（既有语义）：`--config-file` 指向畸形 JSON 时静默回落默认配置，错误可见性为二期候选。
- `StdioAssemblyInput` 新增 `db_path` 字段对 peri-acp 的 crate 外部消费者是 breaking change（内部构造点仅 main.rs 一处，可接受）。
- 测试命令修正：`peri-resources` 侧测试模块为 `context::tests`（`#[path = "context_test.rs"]`），过滤用 `-- open_with`；`test_open_with_none_default_ok` 会触碰真实 `~/.peri/threads/threads.db`（只读查询；无库机器会创建空库，属既有 `open()` 语义）。

### 新增/变更文件汇总

- `peri-tui/src/main.rs`（Cli 两字段、prescan、三路径透传、TuiOptions/run_tui）、`launch.rs`、`app/mod.rs`（App::new Result 化）、`cli_print.rs`、`config/mod.rs`（re-export）
- `peri-acp/src/provider/store.rs`（`CONFIG_PATH_OVERRIDE: Mutex<Option<PathBuf>>` + `set_global_config_path` + `config_path()` 重定向感知；load/save 零改动）、`provider/mod.rs`（re-export）、`host/stdio/init.rs`（StdioAssemblyInput.db_path）
- `peri-agent/src/resources.rs`（`open_thread_store_with` + M-res 注释修正）
- `peri-resources/src/context.rs`（`open_with`）、`context_test.rs`（新）
- 测试：store_test.rs（RAII guard + 6 条 #[serial]）、cli_integration_test.rs（TestCli 镜像 + 10 条）、main_test.rs（prescan 10 条）、context_test.rs（4 条）、resources.rs 内嵌（2 条）
- 文档：README.md（CLI 全局参数段）、docs/top-level.md §8

### 验证

全部通过：peri-tui bin 49 + lib 870、peri-acp 313、peri-resources 44、peri-agent 633 tests；`cargo clippy --workspace --all-targets -- -D warnings`；`cargo build --workspace`；实测 `peri --db-path <不存在目录>/threads.db -p "hi"` → exit 1 且错误含路径。

## 风险与注意

- **env 注入顺序**：`inject_env_from_settings()`（main.rs:373）在 `Cli::parse()` 之前执行，读取的是 `~/.peri/settings.json`——`--config-file` 重定向后该注入不再读取自定义文件。实施时需将 env 注入逻辑移到 parse 之后并按重定向路径执行，或接受"env 注入仅用默认文件"并显式记录。
- **ACP 边界**：thread store 由部署装配点（cli/TUI）注入，ACP 层不直接依赖 Resources（§0）；`db_path` 只出现在装配面与 `StdioAssemblyInput` 可选字段，不触碰 ACP 协议面。
- **`--settings` 语义边界**：新增参数不得与现有 `--settings`（env-only、可接受 JSON 字符串）合并或混淆，文档中需区分。
- 配置结构不变（不加字段），`merge_overrides` 与序列化契约零影响。
