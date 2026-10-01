# 配置系统权威面：现状核查与抽象目标

状态：待设计裁决与实施。2026-10-01。

## 用户目标

配置系统面接受环境变量、各类配置文件与显式部署输入，统一组装后为全局系统提供有效配置；它是配置语义的唯一权威，而不只是通用文件服务。

用户已明确：**配置系统权威面的定义与组装在 Peri 内**。schema、默认值、校验、来源合并、scope、revision 与有效快照均由 Peri 内部 Module 持有；配置 MCP provider 只提供输入 I/O，不获得决定系统有效配置的权限。

## 核查结论：已有输入数据面，未完成有效配置权威面

| 已有 Module | 当前职责 | 与目标的差距 |
| --- | --- | --- |
| `peri-mcp-config` / `ConfigurationMcpServer` | 独立启动通道；文件读取/atomic 写入、路径与同文件身份；本次补按名称读取 provider 环境 | 不解析完整系统配置，不提供统一合并、revision、来源解释或领域投影 |
| ACP `provider::ConfigSource` | 固定全局/工作区来源、`PeriConfig` 加载与合并、差异保存、来源同文件保护 | 是 ACP 设置领域的权威，不包含全部 MCP/部署/环境来源 |
| middleware MCP loader | 全局 settings、插件、项目 `.mcp.json` 合并和 builtin overlay；MCP cache 的 file/env 关闭优先策略 | 与 ACP loader 独立解析输入，生命周期与来源选择不在统一快照中 |
| provider / Langfuse / TUI / resources 配置入口 | 分别解析环境、UI 配置或存储参数 | 业务模块仍可直接读环境、决定默认值与覆盖规则 |

证据入口：`mcp-packages/config/CLAUDE.md` 明确将 typed parsing/validation/precedence 留在 consumers；契约 `peri-acp-types/src/configuration.rs` 当前是输入 I/O 操作。`peri-acp/src/provider/mod.rs::from_env`、`peri-controller/src/langfuse/config.rs`、`peri-middlewares/src/mcp/config.rs` 仍各自解释环境，未收敛为 Peri 内部统一有效快照。不把缺少 MCP Resolve 方法本身当作缺陷：组装不应交给输入 provider。

因此不能把已有 `ConfigurationClient` 宣称为已经完成“全局有效配置单一权威”。本次 MCP cache 实现复用它读取环境，但没有冒充全系统迁移完成。

## 建议抽象与依赖方向

```text
部署参数 / 环境输入 / 文件输入 / 插件配置输入
  → Peri source adapters（文件 I/O 可经现有配置 MCP 数据面）
  → Peri 内部 ConfigurationSystem
       schema/defaults → parse → per-domain merge → validate → publish snapshot
  → typed effective projections
  → ACP / MCP pool / provider / observability / storage / TUI
```

继续使用独立启动配置 MCP 通道作为来源 Adapter，不注册成模型工具，不依赖尚未启动的工具 pool。Peri 内部配置系统不是该 MCP server 的新增业务职责，不要求单独配置服务进程，也不向 source provider 委托组装。

建议以 Peri workspace 内独立 `peri-config` Module 承载定义、纯合并规则与快照，读取来源经 Adapter 注入；`peri-mcp-config` 保持来源 I/O。类型与依赖方向在提取时核对，不能让输入 provider 或配置 Module 反向依赖 ACP/业务消费者。从目前的 `PeriConfig` 和 MCP loader 提取现有规则，迁移一个领域就删除该领域旧解析路径，不保留两套权威。输入 provider 返回的 JSON 或环境字符串仍是待校验输入，不得作为“已解析权威配置”直接发布。

### 小 Interface，深 Implementation

- `resolve(scope, inputs)`：Peri 内部 resolver 接受已采集的来源输入，按 Peri 定义的规则返回 typed 有效快照；纯组装不直接读取环境或文件。Peri 的配置系统负责调用来源 Adapter、发布 revision，消费者不拿原始 JSON 再次组装。
- `explain(scope, field)`：给出来源身份、覆盖/限制原因与生效 revision；敏感值不进入普通解释结果。
- `update(scope, expected_revision, changes)`：校验、保存并发布，失败不改变有效快照。是否支持环境覆盖下的文件编辑及何时生效必须显式表达。

不做任意 source 名/字符串规则组合的通用插件框架，也不假设所有配置字段可以递归 JSON merge。

## 必须保留的语义

1. **“全局”不是“只有一个 cwd 的 map”**：部署基础配置与按项目/执行目录解析的配置视图有不同 scope；同一进程服务多个项目时不得串配置。
2. **每个领域规则只有一份**：profile 整体替换、MetaHarness 逐 key 合并、MCP server 覆盖与 namespace 去重、MCP cache 关闭优先不能被一个统一 last-wins 规则覆盖。
3. **环境所有权显式**：由 Peri 部署装配选择环境来源 Adapter；已有远端配置输入读取 provider 环境，不偷读计算宿主。环境只提供字符串，合法值及合并由 Peri 内部规则决定。某些执行凭据属于工具执行环境，应以显式来源/引用提供，不把所有进程环境混为一份。
4. **不可变快照**：一次 resolve 内使用已采集的输入；发布 revision 后消费者只持 snapshot。文件发生变动不悄悄改写既有 pool/冻结会话。跨文件若无法原子读取，应规定检测/重试，不宣称物理原子性。
5. **动态更新按生命周期处理**：TUI 视图、未来会话、现有 provider、MCP pool、存储部署参数的生效边界各不相同，不能一律“立刻全局广播”。
6. **写权威与读权威统一**：尊重 `--config-file` 与同文件身份保护；保存基于预期 revision 防止覆盖并发编辑；不把环境计算结果反写进文件或意外复制凭据。
7. **可解释但不泄密**：来源标签与规则可观测；token/secret 等字段按敏感分类处理，默认不进入日志、模型工具或普通 UI 投影。
8. **失败明确**：未定义的非法值/不可访问输入不能静默开启能力；保留已裁决的领域容错行为，改变 ACP lenient fallback 等契约需要单独裁决。

## 可独立验收的切片

1. 建立 scope、来源与快照契约，先迁移 MCP cache 的文件/环境解析与解释；与现有关闭矩阵和 pool 冻结回归保持一致。
2. 迁移 MCP server sources、overlay 与 typed 校验；普通/bare/重连/动态接入一致，删除旧 loader 权威路径。
3. 迁移 ACP 设置及保存，保持项目差异保存、同文件保护、model/provider 切换和 frozen 契约；两个项目并发不会串配置。
4. 逐领域接入部署/storage、Langfuse 与 UI；明列“进入权威面”的键，而不是把所有 OS 环境变量都禁止业务访问。
5. 增加 source provenance、revision/update 冲突和远端配置集成验证；迁移完成后审计生产配置读取路径，不只增加一个无人消费的 facade。

验收以消费者收到同 scope/revision 的一致有效值、来源解释、写入冲突和失败行为为准。只统一文件 I/O、只新增 ConfigurationSnapshot 类型或只提供共享 map，都不算完成。

当前未迁移这些全系统领域。目标明确，但字段 schema、变更生效边界及阶段拆分需要确认后执行；本 issue 不作为“配置权威面已实现”的证据。
