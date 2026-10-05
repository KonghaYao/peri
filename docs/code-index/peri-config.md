# peri-config 代码索引

> Peri 内部配置权威面。模块指引见 [peri-config/CLAUDE.md](../../peri-config/CLAUDE.md)，
> 现行设计见 [configuration-authority.md](../design/configuration-authority.md)。

## 数据流与职责

`ConfigurationSource → ConfigurationInputs → assembly::resolve → ConfigurationSnapshot::resolve →
ConfigurationSystem → scoped immutable typed projections`。

source adapter 经选中的配置 MCP provider 读文件正文与具名环境，不读取计算宿主
fallback。纯 resolver 只使用输入值；system 管发布和更新。模块依赖输入/契约能力，
消费者依赖模块，不让核心反向依赖业务消费者。

## 入口

| 要修改的行为 | 文件与符号 | 边界 |
| --- | --- | --- |
| 配置 scope 与稳定 revision | `peri-config/src/system.rs`：`ConfigurationScope`、`ConfigurationInputs`、`ConfigurationRevision` | scope 为绝对 cwd + 全局 settings 路径，revision 覆盖 scope 与输入正文/具名环境 |
| 统一配置实体装配 | `peri-config/src/assembly.rs`：`DOMAINS`、`resolve`、`resolve_mcp`、`environment_keys`、`explain` | 一处声明领域参与来源、合并规则与敏感性，并装配 settings/MCP/provider/观测/UI/资源投影；具名环境采集和来源解释由同一声明驱动，具体合并算法仍由各领域模块持有 |
| 纯 typed 快照 | `system.rs`：`ConfigurationSnapshot::resolve`、`settings`、`mcp`、`provider`、`observability`、`ui` | 多项目隔离，字段只读；Langfuse 使用 global settings，UI 使用合并后的 settings extra |
| 发布、解释与更新 | `system.rs`：`ConfigurationSystem::{resolve,current,explain,update,update_mcp}` | 校验、来源 revision 与目标文件字节 CAS 成功才 publish；失败保留 current |
| 来源输入 | `source.rs`：`ConfigurationSource`、`McpConfigurationSource`、`read_environment`、`collect_with_layout` | 独立 bootstrap；可注入其他 adapter，不委托 provider 决定领域规则 |
| settings schema 与合并 | `app.rs`：`PeriConfig`、`AppConfig`、`Profiles`、`ProfileConfig`、`ProviderConfig` | `merge_overrides`、`extract_overrides`；profile 整体替换、MetaHarness 逐 key 合并 |
| 固定加载/保存布局 | `settings.rs`：`ConfigSource::{load_at,load_standalone,load_lenient,snapshot,reload_merged}`、`save(expected_revision, &PeriConfig) -> Result<Arc<ConfigurationSnapshot>>` | 编辑开始捕获 revision 随草稿提交；保存返回 accepted snapshot。正常持有 system、同文件不拆层；lenient 临时可读/不可写 |
| 受信启动器的内存配置 | `settings.rs`：`ConfigSource::load_injected_at`；`source.rs::collect_with_layout_and_global` | 以内部启动帧提供完整 global settings 正文；不读取或写入 global/workspace settings 文件，仍使用同一 typed snapshot 与 provider/MCP 规则；该来源拒绝持久保存 |
| 单文件 helpers | `settings.rs`：`load_from`、`save_to` | `save_to` 显式路径也用字节 CAS 保留其他顶层领域；不发布 scoped snapshot，不等同于 system revision 更新。lenient source 无 authority 时临时可读、不可写 |
| 资源配置投影 | `resources.rs`：`ResourceConfiguration`、`resolve`；`system.rs::resources` | global `config.disableBundledSkills` 优先于旧顶层键，默认 false；workspace 资源 consumer 使用 snapshot 开关 |
| Provider 解析 | `provider.rs`：`resolve`、`resolve_for_alias`、`ResolvedProvider`、`ENVIRONMENT_KEYS` | `MODEL_PROVIDER` + `MODEL_TYPE` 选择配置的 provider ID 和档位；缺省用 active profile；消费者负责 Model adapter 构造 |
| MCP 基础配置 | `mcp.rs`：`parse_global`、`parse_project`、`resolve_from_files`、`validate_config`、`server_config_hash` | global/plugin/project 合并、插件手动去重、typed 准入与 cache 关闭优先 |
| Builtin 环境策略 | `mcp.rs`：`builtin_enabled` | `PERI_MCP_BUILTIN` 解释规则归 core；旧 builtin adapter 只采集 env，不复制规则 |
| 快照中的插件/普通/bare 投影 | `system.rs`：`mcp_with_plugins`、`bare_mcp`、`cache_policy`、`builtin_mcp_enabled` | 从冻结原始输入派生；插件发现和执行展开留在 middleware |
| Langfuse | `observability.rs`：`LangfuseConfig`、`resolve`、`ENVIRONMENT_KEYS` | 纯 settings/env 解析，保持 clamp/default/invalid/batch 规则；Debug 脱敏 |
| TUI | `ui.rs`：`TuiConfig::{from_extra,sync_to_extra}` | UI 默认值、typed 字段和 extra 保存规则；视图状态留在 TUI |

## 更新接纳边界

- settings 更新保留目标原文 nested `config.mcpServers` / `config.mcpCache`，
  only-provider 请求不能删除；workspace 不从 effective global 导入这些键，
  已有 workspace `$schema` 优先。提交后使用 accepted snapshot，不能发布候选草稿。
- `update_mcp` 只接受显式无来源输入或本 cwd `.mcp.json` 的 Project server；
  拒绝 Global / Plugin / Builtin / 其他 cwd.Project，防止 merged 全局凭据落入项目。
- expected revision 是编辑开始的基线，提交时不能改取最新版。public core 接口
  已修复；TUI `save_effective` 仍在提交时取 revision，延迟 draft / 远程 wire token
  接线在 active issue 保持 followup，不宣称端到端版本保护已完整闭环。

## Consumer 接线

- ACP `host/requests/config_options.rs` 持久字段按 candidate → 验证 → 保存 →
  accepted snapshot 发布；失败不改 live provider / agent cache、不 notify、不 success。
  回归入口 `host/requests_config_options_test.rs`。
- ACP 的 `provider/{config,store}.rs` re-export core 类型与 `ConfigSource`；
  `provider/mod.rs` 将 `ResolvedProvider` 转成 `LlmProvider` 和具体 Model adapter。
- `peri-acp/src/host/assemble.rs` 将 `ConfigSource::snapshot` 注入新建 MCP pool，
  并从同一 snapshot 获取 host Langfuse 配置。
- `peri-middlewares/src/mcp/client.rs::set_configuration_snapshot` 在初始化前只写一次，
  `configuration_revision` 可核对绑定版本；`initialize.rs` 选择 snapshot loader。
- `peri-middlewares/src/mcp/config.rs` 只做插件采集/执行参数展开/builtin overlay；
  基础合并与校验委托 core。snapshot 目录写法不同时，经 `io::same_file` 核对同目录，
  不重读冻结配置；回归见 `mcp/config/snapshot_test.rs`。未注入 snapshot 的 adapter 仍调用 core 规则。
- Controller `langfuse/config.rs` re-export `LangfuseConfig`；TUI
  `config/tui_config.rs` re-export `TuiConfig`，`kit/entry.rs` 优先从 snapshot 初始化。
- ACP `host/workspace.rs` 用 `snapshot.resources().disable_bundled_skills` 构造
  workspace 资源输入；正常 snapshot 路径不重新读取全局关闭位。技能回归 fixture
  `requests_skill_resources_test.rs::skill_server_config` 使用选中的 global 配置路径。

## 验证入口与限制

- `system_test.rs`：scope/revision、隔离、冻结、失败不 publish、stale revision、
  外部环境/文件变更、CAS/I/O 失败、兄弟域保留、global 失效与脱敏解释。
- `settings_test.rs`：固定布局、同文件身份、差异保存、显式 reload、lenient
  错误禁止写错层、unknown 顶层字段保留。
- `app_test.rs`、`mcp_test.rs`、`observability_test.rs`、`ui_test.rs`、`resources_test.rs`：领域规则。
- `mcp-packages/config/src/cas_test.rs`：missing/empty 区分、冲突、并发 CAS 与 wire。

典型命令：`cargo test -p peri-config --lib -- system::tests`。
实际验收记录及独立 pool 接线、跨进程/远端矩阵见
[active issue](../../spec/issues/2026-10-01-configuration-authority.md)。输入读取非跨文件
事务，CAS 不保护不合作编辑器；旧 pool 与 session prefix 不会自动热更新。

## 配置 I/O 适配

`peri-config::io` 是配置读取与写入的部署边界。原生目标转发给 `peri-mcp-config` 的 MCP 数据面；Emscripten 目标在虚拟文件系统中读写文件，因此 WASM 依赖图不引入 `mcp-packages/config`。
