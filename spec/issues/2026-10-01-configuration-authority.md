# 配置系统权威面：核心实现与后续边界

状态：核心权威面已实现；consumer 接线验收与长期专属领域扩展仍 active。2026-10-01。

现行规则见 [配置权威面设计](../../docs/design/configuration-authority.md)，入口见
[peri-config 索引](../../docs/code-index/peri-config.md) 与
[核心模块指引](../../peri-config/CLAUDE.md)。本 issue 只保留仍需实施或验收的工作；
源码与测试入口的存在不等于本轮已运行通过。

## 用户目标与已实现范围

配置语义的定义与组装在 Peri 内。来源 MCP 只提供 byte/env/path 输入与字节 CAS，
不定义默认值、来源优先级或系统有效配置；计算宿主不读取本机 fallback。

| 核心能力 | 当前代码事实 |
| --- | --- |
| source adapters | `ConfigurationSource` / `McpConfigurationSource` 采集固定 layout 的文件正文与具名环境；独立 bootstrap，不依赖待配置工具池、不增加 daemon/model 工具 |
| 纯 typed 权威 | core app/settings/provider/MCP/observability/UI/resources 持有 schema/defaults/validation/domain merge，`ConfigurationSnapshot::resolve` 不做 I/O |
| scope 与版本 | 绝对 cwd + 选中 global settings 路径；revision 由 scope 与输入内容确定，多个项目隔离 |
| 发布 | `ConfigurationSystem` 管 scoped immutable `Arc` snapshots，resolve/update 成功才 publish，失败保留 current |
| 解释与安全 | 领域级 contributors/rule/revision/sensitivity，不输出原始值；输入/snapshot/provider/Langfuse Debug 脱敏 |
| 更新 | `update` / `update_mcp` 核对 expected revision、重新采集、解析候选、目标字节 CAS 后发布；global 更新失效其他共享路径的 current 项 |
| 保存 | `ConfigSource::save(expected_revision, &PeriConfig)` 返回 accepted snapshot；token 在编辑开始捕获。settings 保护 nested MCP-owned 键与 workspace schema，避免 only-provider 请求删除或把 effective global 键复制到 workspace；保留顶层兄弟域 |
| Settings consumer | 正常 core `settings::ConfigSource` 正式持有 `ConfigurationSystem`；ACP re-export；lenient 临时可读/不可写，无 authority 的旧 merge/save fallback 已删除 |
| ACP/MCP 接线 | `host/assemble.rs` 从同一 source snapshot 绑定新建 pool 与 host Langfuse；pool 只在 init 前绑定一次，普通/bare snapshot loader 委托 core |
| 资源开关 | `resources::ResourceConfiguration` / `snapshot.resources().disable_bundled_skills` 投影选中 global 来源的关闭位；workspace 资源实际 consumer 不再重读全局；技能 fixture 与该 global path 一致 |
| Builtin 开关 | core `mcp::builtin_enabled` 解释 `PERI_MCP_BUILTIN`；旧 builtin adapter 只采集 env 并委托统一规则 |
| Provider/观测/UI | provider defaults/profile/alias/environment fallback 与 Langfuse/UI typed projections 归 core；Model、tracer、UI atoms 留在消费者 |
| MCP runtime | middleware 只负责插件发现、执行展开与 builtin runtime overlay，基础来源合并、typed 准入、去重与 cache 规则委托 core |
| 来源写入保护 | `WriteTextIfUnchanged` 比较 expected 正文字节，missing/empty 不同；比较与 atomic replacement 使用进程锁及跨进程目标文件锁 |

保留领域语义：profiles 整体替换、MetaHarness 逐 key 合并；provider settings profile
优先、环境 fallback；MCP global → plugin → project、手动去重插件；cache 任一 false
关闭；Langfuse global settings 后环境覆盖并保留 clamp/invalid/default/batch 行为。

## 已知保证边界

- 采集多个文件/环境不是跨来源原子事务；revision 标识已采集输入，不证明共同时间点。
- byte CAS 只协调合作写者，外部不合作编辑器可在比较与替换窗口内写入；跨目标路径
  或多文件写入不在锁事务保证内。超时写入可能已进入 OS I/O。
- 解释当前为领域级来源贡献，不是每个标量的精确覆盖胜者或完整来源证明。
- 显式 reload/save 不热替换既有 immutable pool，不重写已冻结 session prefixes；
  已持有旧 Arc 的消费者继续使用原 revision。须显式 reload 并重取新 snapshot，
  旧 pool 固定旧 Arc，没有 hot watcher；宿主操作决定后续 provider/UI/新会话生效。
- `save_to` 的显式路径也已改为字节 CAS，并保留 siblings；它不发布 scoped snapshot，
  不等同于带 expected scoped revision 的 system 更新。
- 配置 provider 的具名环境 ownership 不代表禁止业务访问全部 `std::env`；执行凭据
  与 OS 运行环境应由执行能力明确提供，不能偷换成配置宿主环境。

## 本轮审查修复与开放项

- [x] public core 保存 API 明确接受 expected revision 并返回
  `Arc<ConfigurationSnapshot>`；caller 必须保留编辑开始的 token，不在提交时取最新版。
- [x] settings 更新保护目标原文 nested `config.mcpServers` / `config.mcpCache`；
  only-provider 请求不删除 MCP-owned 键，workspace 不从 effective global 导入这些键，
  已有 workspace `$schema` 优先保留。
- [x] `update_mcp` 拒绝 Global / Plugin / Builtin / 其他 cwd.Project server 来源；
  显式无来源或本项目 Project 输入才可更新，避免 merged projection 导入全局凭据。
- [x] ACP 持久 configOptions / update_config 已改 candidate → 验证 → 保存成功 →
  accepted snapshot 发布；失败不更新 live provider / agent cache、不 notify、不 success。
  回归入口：`peri-acp/src/host/requests_config_options_test.rs`。
- [ ] 旧 UI 延迟 draft 的版本 token followup：`save_effective` 仍在提交时读取
  snapshot revision，当前仅为串行当前视图便利入口；延迟编辑器应从编辑开始捕获
  基线并传入 source.save。审计远程 wire 携带 expected revision，不能以服务端处理
  请求时现取 token 替代客户端原始草稿基线。public core 与 ACP 失败顺序修复不代表
  延迟编辑的端到端 CAS 已完成。

## 待做：consumer 接线与写路径审计

- [ ] 独立 TUI MCP panel pool：当前 `App::spawn_mcp_init` 创建的 pool 尚未调用
  `set_configuration_snapshot`，无快照分支仍经 adapter 委托 core；确定并接入与宿主
  source 相同的 scoped revision 后，再声称所有部署 pool 均绑定同一快照。
- [ ] 审计 `remove_server_from_config` / `set_server_disabled` 等 MCP 消费者写入口：
  当前 middleware 的 atomic helpers 不能等同于 `ConfigurationSystem::update_mcp`；
  核对 expected revision、global/project 写层及兄弟域保留后完成权威更新接线。
- [ ] 核对 TUI/print/stdio、不同 cwd session、普通/bare、重连/OAuth/动态/ACP bridge
  的来源和 revision 一致性；共享旧 pool 不因新 snapshot 发布而改变。
- [ ] 由架构协调者同步 import 检查声明：core 向 ACP/Middleware/Controller/TUI 提供，
  core 只依赖共享契约与来源输入。文档声明不替代实际 import gate 验收。

## 待做：专属领域扩展

这些是后续扩展项，不把既有专属能力误报成已迁移：

- [ ] LSP 的配置规则、host pool 构造与关闭生命周期，明确来源/scope 后评估迁入。
- [ ] 插件安装、市场、enabled 状态与生命周期；插件 MCP 输入已交 core 合并，
  不意味着插件管理本身成为 snapshot owner。
- [ ] Hook 的宽松格式与来源优先级；既有输入 I/O 已经复用配置 MCP，格式权威仍专属。
- [ ] OS 执行环境与工具 credentials 的来源、信任及生命周期；不得把所有环境混一份。
- [ ] 存储 locator、远端连接 credentials 与部署参数；不让 settings 快照替代存储 owner。
- [ ] 若需精确字段来源/环境覆盖下的编辑解释或热更新，先定义新语义与生效边界，
  不把当前 explain 或显式 reload 包装成已完成能力。

## 待验收与证据入口

现有测试入口覆盖 core scope/revision、隔离、冻结、失败不 publish、stale 输入、
CAS/I/O 失败、兄弟域保留与脱敏；settings 测试覆盖固定路径、同文件身份与差异保存。
来源 CAS 测试包含 wire、missing/empty 与同进程并发。跨进程文件锁实现存在，不据此
宣称独立 OS 进程争用或远端部署矩阵已验证。

```bash
cargo test -p peri-config --lib -- system::tests
cargo test -p peri-config --lib -- settings::tests
cargo test -p peri-config --lib -- mcp::tests
cargo test -p peri-config --lib -- observability::tests
cargo test -p peri-config --lib -- ui::tests
cargo test -p peri-config --lib -- resources::tests
cargo test -p peri-mcp-config --lib -- cas_test
cargo test -p peri-middlewares --lib -- mcp::config::snapshot_tests
cargo test -p peri-acp --lib -- host::requests::tests::skill_resources
cargo test -p peri-acp --lib -- host::requests::tests::update_config_tests::config_options_tests
cargo test -p peri-acp --lib
```

- [x] 协调者于本轮反馈：先前 3 个失败已修复，全部定向测试 passed；ACP skill
  fixture 已使用选中的 global 配置路径，相关入口为 `requests_skill_resources_test.rs`。
  本次文档同步未重跑代码测试，该反馈不扩展为 workspace 全量或远端矩阵通过。
- [x] 协调者最新反馈：core tests 当前 118 passed。本轮文档同步未执行代码测试，
  该数字按协调者反馈记录，不代替最终跨 crate 验收。
- [ ] ACP 最终验证须重跑：此前 724 passed 的全量结果发生在本次 configOptions
  修复之前，仅为旧基线，不能写成修复后最终通过。等待父 agent 提供最终结果，
  再记录最终代码对应的退出状态与实际执行用例数。
- [ ] 协调者汇总 final exit status、实际目标用例数与最终 worktree 对应证据；本 issue
  不补造未提供的总数或完整门禁结果。
- [ ] 用真实独立进程证明合作 CAS 单 winner；覆盖失败/冲突与现有正文保持不变。
- [ ] 远端配置 provider 环境与计算宿主不同：验证 provider 值生效、不可得不本机 fallback，
  输入和错误/Debug/普通 explain 不泄露 secret。
- [ ] 两项目并发、文件/环境变更、shared global 失效、reload/save 与既有 pool/session
  生命周期验证；失败不 publish，global credentials 不复制到 workspace。

核心权威面已经落地。本 issue 保持 active，是因为这些部署接线、验收与专属领域
扩展仍未全部闭环；不再以“只有统一文件 I/O、尚无配置权威”描述现状。
