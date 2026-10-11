# peri-config

## Scope

`peri-config` 是 Peri 内部配置权威面：持有 settings、provider、MCP 基础配置、
Langfuse、UI 与资源开关的类型、默认值、解析、领域合并及有效快照。消费者只适配运行能力，
不重新解释这些领域的来源优先级。配置来源 MCP 提供输入 I/O，不决定有效配置。

开始任务时显式读 `../docs/standards/{architecture-contracts,rust,testing,documentation}.md`。
现行设计见 `../docs/design/configuration-authority.md`；入口见
`../docs/code-index/peri-config.md`；扩展与验收缺口见
`../spec/history/2026-10.md`（2026-10-01 条目）。

## 数据流

```text
选中的 ConfigurationSource / source adapters
  → ConfigurationInputs（文件正文 + 具名环境输入）
  → assembly（来源形状、领域装配与 typed 投影）
  → ConfigurationSnapshot::resolve（冻结结果）
  → ConfigurationSystem（按 scope 发布 Arc 快照、revision、explain、update）
  → ACP / MCP pool / Controller / TUI
```

默认 `McpConfigurationSource` 经独立 bootstrap 配置 MCP 采集输入；不经过待配置的
session 工具池，不新增 daemon 或模型工具。环境来自选中的配置 provider，
远端输入不可得时不回落计算宿主环境或文件系统。

## 任务路由

| 任务 | 入口 |
| --- | --- |
| scope、revision、快照、发布、解释与 CAS 更新 | `src/system.rs`、`src/system_test.rs` |
| 领域来源形状、具名环境采集键、装配与来源解释 | `src/assembly.rs`、`src/assembly_test.rs` |
| 来源采集、具名环境、bootstrap I/O | `src/source.rs`、`../mcp-packages/config/` |
| settings 类型、profiles、MetaHarness 规则 | `src/app.rs`、`src/app_test.rs` |
| 固定配置布局、差异保存、显式 reload | `src/settings.rs`、`src/settings_test.rs` |
| provider 默认值、alias/profile、环境档位选择 | `src/provider.rs` |
| MCP 解析、来源合并、去重、cache 关闭规则 | `src/mcp.rs`、`src/mcp_test.rs` |
| Langfuse typed projection | `src/observability.rs`、`src/observability_test.rs` |
| TUI typed projection、extra 读写 | `src/ui.rs`、`src/ui_test.rs` |
| 资源配置开关 | `src/resources.rs`、`src/resources_test.rs`；`snapshot.resources()` |
| beta flag 覆盖与投影 | `src/betas.rs`、`src/betas_test.rs`、`src/app.rs`（`BetasConfig`）；`snapshot.flags()`、`ConfigSource::beta_flags()` |

## 稳定不变量

- 核心依赖契约 `peri-acp-types` 与输入能力 `peri-mcp-config`；不反向依赖 ACP、
  Middleware、Controller 或 TUI 的业务实现。适配器与纯解析分开。
- `ConfigurationScope` 包含绝对 cwd 与选中的全局 settings 路径；不同项目不可
  共用一个无 scope 的有效配置 map。revision 由 scope 和已采集输入内容确定。
- 正常 `settings::ConfigSource` 持有 `ConfigurationSystem` 与 scope；ACP store
  仅 re-export。lenient 失败降级仅临时可读、不可写；无 authority 的旧合并/保存
  fallback 已删除，不可据此声称已发布有效快照。
- `resolve` 成功才发布；失败保留 current。快照只提供只读 typed 引用；旧 `Arc`
  不因显式 reload、保存或来源文件变动而改变。
- settings 中 profiles 整体替换、MetaHarness 逐 key 合并；provider 在未指定模型环境组合时使用 active profile；`MODEL_PROVIDER` 与 `MODEL_TYPE` 成对选择配置中的 provider ID 和档位。这些规则不能替换成通用递归 merge。
- MCP 规则由 core 单一维护：global → plugin → project、手动配置去重插件、
  任一来源 false 关闭 cache。middleware 负责插件发现、执行参数展开与 builtin
  runtime overlay，不另建文件/环境优先级。
- `mcp::builtin_enabled` 唯一解释 `PERI_MCP_BUILTIN`，旧 builtin adapter 只采集具名
  环境并委托 core。`resources::ResourceConfiguration` 投影 global 的
  `disableBundledSkills`；workspace 资源 consumer 使用 snapshot，不重新读全局关闭位。
- `explain` 当前按领域列出贡献来源、规则与 revision，不返回 secret；它不是
  每个标量的精确胜者追踪。Debug 不输出原始输入或凭据。
- `update` / `update_mcp` 检查预期 revision、重新采集、解析候选，再做目标文件
  字节 CAS；冲突、校验或 I/O 失败不发布。保存只改负责的键，保留同文件兄弟域。
- `ConfigSource::save(expected_revision, &PeriConfig)` 返回接纳后的
  `Result<Arc<ConfigurationSnapshot>>`。caller 在编辑开始从基线 snapshot 捕获 token，
  保存时提交同一 token，不得临提交取最新 revision 来掩盖 stale draft。
- settings 更新保护目标原文中 nested `config.mcpServers` / `config.mcpCache`；
  only-provider 请求不能删除这些领域-owned 键，也不能从 effective global 把它们
  复制进 workspace。已有 workspace `$schema` 优先保留。
- `update_mcp` 只接纳无来源的显式项目输入或本 cwd `.mcp.json` 的 Project 来源；
  拒绝 Global / Plugin / Builtin / 其他 cwd.Project，防止 merged projection 中的
  global credentials 被写到 project。
- ACP 持久 configOptions 分支先构造 candidate、验证，再保存；成功后从 accepted
  snapshot 发布。失败不修改 live provider / agent cache、不 notify、不返回成功。
  TUI `save_effective` 仍是提交时取 revision 的串行当前视图便利入口，不能充当
  延迟草稿的编辑基线；延迟编辑器与远程 wire token 接线仍须审计。
- 全局配置更新会失效共享该全局路径的其他 current 项；已经持有的快照仍有效，
  其他 scope 须显式 resolve。MCP pool 初始化前只绑定一次快照，不提供热替换。
- 输入采集不是跨文件原子读取；文件锁只协调合作写者，不能承诺阻止外部编辑器
  在检查与替换之间写入。底层 atomic API 不等于带 revision 的权威更新。
- `save_to` 的显式单文件路径也使用字节 CAS 并保留 siblings；它不发布 scoped
  system snapshot。必须显式 reload 并重新取得快照，不存在 hot watcher。
- 插件生命周期、hook 格式、OS 执行环境、存储 locator 与 credentials
  仍有专属能力边界；禁止重复解释已迁移领域不等于禁止业务使用所有 `std::env`。

## 目标命令

```bash
cargo check -p peri-config
cargo test -p peri-config --lib -- system::tests
cargo test -p peri-config --lib -- settings::tests
cargo test -p peri-config --lib -- mcp::tests
cargo test -p peri-config --lib -- observability::tests
cargo test -p peri-config --lib -- ui::tests
cargo test -p peri-config --lib -- resources::tests
cargo test -p peri-mcp-config --lib -- cas_test
git diff --check
```

测试存在不代表已执行；验收证据遵循 `TEST-EVIDENCE-001`。跨部署接线和长期扩展
清单在 active issue 维护，不在本指引复制进度或历史。
