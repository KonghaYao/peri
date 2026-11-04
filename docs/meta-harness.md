# MetaHarness：提示词与能力配置

> 非权威使用指引。机制与契约以 [MetaHarness 设计](design/meta-harness.md) 为准；
> 在线使用文档见 [文档站](https://konghayao.github.io/peri-cool/)。

MetaHarness 通过 `settings.json` 的 `config.meta_harness` 配置系统提示词覆盖、
middleware 关闭与内置 Agent 定义策略。

## 快速开始

在全局 `~/.peri/settings.json` 或项目 `{cwd}/.peri/settings.json` 中配置：

```json
{
  "config": {
    "meta_harness": {
      "01_intro": true,
      "05_using_tools": true,
      "WebMiddleware": false
    }
  }
}
```

创建 `.peri/meta/01_intro.md` 与 `.peri/meta/05_using_tools.md`，分别写入完整替换正文。
配置中的 `WebMiddleware: false` 关闭本会话的内置 Web 工具能力。

| 配置项 | 用法 | 事实源 |
| --- | --- | --- |
| 段落 ID = `true` | 用 `.peri/meta/<ID>.md` 整段替换；空正文移除该段 | `peri-acp-types/src/meta_harness.rs::SECTION_IDS` |
| middleware 名 = `false` | 不装配该 middleware，工具、hook 与提示词贡献随之移除 | 同文件 `MIDDLEWARE_NAMES` |
| builtin 实例策略键 = `false` | 关闭该会话的实例能力面；具体关闭范围按设计约束 | `peri-acp-types/src/builtin_mcp.rs` 的 `policy_key` |
| `BuiltInSubagents` = bool | 控制编译时内置 Agent 定义，默认开启 | [MetaHarness 设计](design/meta-harness.md) |

`BuiltInSubagents: false` 不关闭项目/plugin agents、fork/resume 或 Agent 工具。
覆盖不能创建已关闭 middleware 不再持有的段落。

## 配置与生效时机

- 全局与项目配置按 key 合并，项目同名 key 覆盖全局值。
- 未知 key 记录 warning 后忽略；请使用声明表中的现行名称。
- 配置与覆盖正文在 `session/new` 冻结期读取，会话内不热更新。
  修改配置或文件后创建新会话；子 Agent、fork 与 workflow 子面复用冻结状态。
- 覆盖正文经受信 workspace 的 `peri-meta://workspace/<ID>` 资源读取。
  workspace 不可用、正文缺失或读取失败时告警并保留内置段落，不回落计算实例本机读盘。
- 全文替换保留段落原有渲染位置；空白正文不做 trim。

## 能力与连接管理

MetaHarness 管理会话内的能力使用策略。安装外部 MCP、连接/授权管理、响应缓存与
工具执行权限各自使用对应配置，不通过自造 middleware 名控制。

| 需求 | 配置入口 |
| --- | --- |
| 关闭内置能力的会话注入 | 对应 builtin 实例策略键，如 `WebMiddleware: false` |
| 在配置层禁用内置 MCP | 对应 MCP 配置片段，如 `{"web": {"disabled": true}}` |
| 全局关闭内置 MCP 注入 | `PERI_MCP_BUILTIN=off` |
| 关闭 MCP 响应缓存 | `mcpCache` 或 `PERI_MCP_CACHE`，见 [缓存设计](design/mcp-cache.md) |
| 控制工具执行权限 | 会话工具视图、allowlist/disallowlist 与运行时审批 |

普通策略关闭不销毁 pool 连接或后台调度。workspace 资源关闭、workflow 物理关闭等
特例见 [关闭契约](design/meta-harness.md)；不要用实例是否仍显示在面板中判断注入是否开启。
`PermissionMiddleware` 管理审批，`HumanInTheLoopMiddleware` 管理提问，两者独立。

覆盖安全段落会改变模型的行为约束；关闭基础提示词 middleware 可得到空系统提示词。
使用时先确认要调整的段落或能力；提示词覆盖本身不授予工具权限。

## 进一步阅读

- [MetaHarness 设计](design/meta-harness.md)：关闭闭包、冻结来源及配置规则。
- [System Prompt 设计](design/system-prompt.md)：提示词冻结、缓存与安全边界。
- [MCP 生态参考](reference/mcp-ecosystem.md)：实例配置与连接生命周期。
