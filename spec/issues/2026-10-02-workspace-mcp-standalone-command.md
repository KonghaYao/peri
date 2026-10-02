# 独立 Workspace MCP 进程：`peri mcp-start workspace`

状态：开发中。用户已确定进程边界：`peri` 是命令分派器，`mcp-start workspace` 启动独立、纯 MCP 的工作进程。CLI/worker、运行时任务边界和定向 wire 测试已落地；发行包、TS SDK 联动与完整集成验收仍待完成。本文其余未验收能力仍是目标设计。

当前开发状态：项目使用 `rmcp 3.5.0`，其发布版本的 `SubscriptionFilter` 尚无 Tasks 扩展的 `taskIds`，`SubscriptionSink` 尚不转发 `notifications/tasks`。[项目补丁](../../patches/rmcp-3.5.0-task-subscriptions.patch)覆盖任务 ID 过滤、客户端能力与任务访问校验、通知发送与接收；通过 [构建脚本](../../scripts/cargo-rmcp-patched.sh)从已校验的 crates.io 发布包重建本地 patched crate，具体命令见 [补丁说明](../../patches/README.md)。仓库不提交第三方源码，也不依赖 GitHub fork。Workspace 已提供 `tasks/get`/`tasks/cancel`、可查询状态和任务订阅；Peri 的工具桥接已消费任务回执与通知，重启后跨进程恢复仍未实现。

## 目标与现状

目标命令：

```bash
peri mcp-start workspace --stdio --workspace /abs/workspace
peri mcp-start workspace --http --workspace /abs/workspace --bind 127.0.0.1:8765
```

执行后处理 MCP 请求的是单独发布的 `peri-mcp-workspace` 可执行程序。它不启动 Peri 的 TUI、ACP Agent、RCRA、模型、Session Store、插件宿主或会话配置装配。`peri` 只解析并转交该子命令；普通 `peri` 与 `peri acp` 的行为保持原有入口。stdio 与 HTTP 是**同一份 Workspace MCP handler** 的两种 transport，不产生第二套工具和资源实现。

[`WorkspaceMcpServer`](../../mcp-packages/workspace/src/workspace.rs) 由 [builtin runtime](../../peri-middlewares/src/mcp/builtin/runtime.rs) 在 Peri 进程内通过 duplex 启动，也由独立 `peri-mcp-workspace` 进程承载；[`peri` CLI](../../peri-tui/src/main.rs) 已有 `mcp-start` 分派入口。历史 `local-mcp-server` 在提交 `2b780b2a` 中退役，不能恢复其独立的工具/schema 权威。已有 [v4 MCP 设计](../../docs/design/mcp-adaptation-v4-part-1.md) 允许 external stdio/HTTP；本 issue 仍需完成独立宿主的发行与端到端验收。

## 模块与进程边界

```text
peri（薄命令分派器）
├── 无子命令 / acp / 其他既有命令 → 现有 Peri 入口
└── mcp-start workspace → peri-mcp-workspace（独立进程）
                           ├── WorkspaceMcpServer：七工具与完整资源面
                           ├── Workspace 后台 Bash owner / MCP Tasks 扩展
                           └── stdio 或 Streamable HTTP MCP transport
```

1. 在任何 Peri 全局 settings、会话存储、Agent runtime 或 TUI 初始化之前识别 `mcp-start`。分派器只定位同发行包内的 `peri-mcp-workspace`，转交参数和退出状态；找不到二进制明确报部署错误，不回退为进程内 builtin。Unix 可 `exec` 替换进程映像；不支持 `exec` 的平台由薄分派器等待子进程，实际 MCP 处理仍只在独立工作进程。
2. `mcp-packages/workspace` 提供该二进制入口和唯一 handler 工厂。transport、工作区启动配置与关闭 owner 属于该包的独立运行时；不从 `peri-middlewares` 引入 builtin dispatch/context，也不调用 `peri-acp`。现有包和 `peri-mcp-common` 对 `peri-agent` 的类型/任务辅助依赖需审计：共享契约可下沉，但不得为了独立入口启动 Agent 运行时或引入另一份工具行为。发布物需包含分派器及 Workspace MCP 二进制。
3. 一个 Workspace MCP 进程固定一个 canonical 工作区根。根与资源根在启动时确定，MCP 请求不能改写根；一个 HTTP 服务不能用请求参数动态选择另一个 Sandbox。工作区路径约束与 Bash 实际权限须按现有工具语义如实表述，不能把 `--workspace` 称为 OS 沙箱。
4. stdio 的 stdout 只写 MCP JSON-RPC，stderr 写脱敏诊断；stdin EOF、信号和 transport 关闭后收口工具任务。HTTP 在 `/mcp` 提供 Streamable HTTP，先支持显式 `--bind`；非回环监听须配置认证来源，凭证不进入命令行、URL 或日志。HTTP 的就绪判据是实际 MCP 初始化成功，不是端口已绑定。

## Workspace 能力的独立装配

**运行时边界进度（2026-10-02）**：独立 CLI 与 builtin dispatcher 已统一使用 Workspace 自有的 `ShellTasks`；ACP 会话装配不再传入 session `TaskManager` / 完成回调。后台 Bash 可经 MCP Tasks 查询、取消与按 `taskIds` 订阅完成状态，Peri 的工具桥接接受任务回执并为发起会话建立订阅。当前订阅与 task ID→session 关系保存在 Peri 进程内；重启/重连后的 task ID 持久关联与重新订阅对账仍待实现。`WorkspaceInstanceInput` 仍保留在直接构造器及旧工具测试中，`mcp-packages` 对 `peri-agent` / `peri-acp-types` 的 Cargo 依赖尚未清理；这是后续编译边界工作，不应把本轮运行时解耦说成所有依赖已经去除。

`WorkspaceMcpServer::new(cwd, input)` 当前由 Peri 会话注入后台 Bash 的 `WorkspaceInstanceInput`（session `TaskManager` 和 `on_bg_complete` 回调），资源面还需显式 `with_resources(WorkspaceResourcesInput)`。只调用 `new(cwd, None)` 虽能列出七工具，却会使后台 Bash 不可用；不调用 `with_resources` 则 Skill、Agent 定义、项目指令和 MetaHarness 等资源面缺席。目标是消除这两类 **Peri 会话对 Workspace 的装配输入**：

- Workspace 进程自己持有**后台 Bash** 的任务登记、进程执行、状态、取消和关闭清理；Peri 不把 session `TaskManager` 或 `on_bg_complete` 闭包传入该进程。Agent/Workflow 等 Peri 自有任务暂留原所有者，不能为了 Bash 解耦一并迁走。共享的 Shell 执行、输出和任务契约要按实际依赖下沉，避免独立 Workspace 运行时仍靠 `peri-agent::agent::async_tasks::TaskManager` 才能启动。移除 `WorkspaceInstanceInput` 的 session 语义和后台 Bash 缺 manager 时的降级分支后，七工具在两种 transport 下均真实可用。
- 后台 Bash 采用 MCP 2026-07-28 的 `io.modelcontextprotocol/tasks` 扩展：创建任务返回 `CreateTaskResult`；`tasks/get` 读取完整状态和最终结果，`tasks/cancel` 请求取消；客户端用 `subscriptions/listen` 的 `taskIds` filter 接收 `notifications/tasks`。订阅只传变化，不能代替可查询状态；断流重订阅后按已保存的 task ID 用 `tasks/get` 对账。`notifications/tasks` 是标准任务通知，不混作 `notifications/resources/updated`。Workspace 按协议声明能力、核验调用者访问任务的权限，并在返回 task ID 前保证任务已可查询。[^mcp-tasks]
- Workspace 只按部署时确定的可见范围提供 `resources/list|read|templates/list`、`skills/list|get` 等标准获取接口及内容；是否读取、启用、注入 prompt、冻结或呈现，由**外部 MCP client** 决定。Peri 只是其中一个 client，它的 Skill 聚合、批准、来源信任和 prompt 冻结属于 Peri 客户端策略，不成为 Workspace server 的策略或通用协议规则。资源发布根允许启动时显式传入；未传入时采用 Workspace 默认目录：工作区内的项目指令、项目 Skill 与 Agent 目录，用户 Skill 目录及可用的 builtin 资产。插件根没有通用默认来源，只有显式传入才发布。**未传**与**显式空列表**要区分：后者表示不发布该类外部目录。现有 [`WorkspaceResourcesInput`](../../mcp-packages/workspace/src/resources/mod.rs) 中的根、builtin 开关和读取预算属于 server 可见范围/服务上限；Peri 的注入决策不得混入其中。
- `tools/list`、完整资源面、Tasks 扩展、订阅和现有 Workspace custom request 均由同一 handler 提供；stdio/HTTP 不复制 schema、错误映射或 capability 声明。

## 与 TS SDK / Peri 的关系

HTTP 模式下，Sandbox 持有 Workspace MCP 进程与 URL，生命周期独立于某个 Agent/Session；`Session.start()` 须待其 MCP 协议握手可用，再将 URL 声明为会话级 HTTP `workspace`。Peri 只运行 Agent 并作为 MCP client 消费该外部 Workspace；显式外部来源应替换同名 builtin 来源，`tools/list` 是其 direct 工具清单权威，资源面也来自该连接。Peri 保存由工具调用返回的 task ID 与本地会话/呈现关系，按协议订阅与查询状态；它不成为该 Bash 的任务 owner。MCP Tasks 扩展没有 `tasks/list`，重连后不能靠列举找回 task ID。Agent 关闭不自动关闭 Sandbox 的 Workspace 进程。[^mcp-tasks]

stdio 模式下，可由 MCP client 根据 `command + args` 启动该二进制，连接退出即关闭该 MCP 进程；它适合本地单连接，不承担 HTTP 模式的独立驻留。SDK demo 若演示 Workspace，只启动已打包的命令，不在 Bun 中重写 Workspace MCP handler。现有 demo 的 Bun HTTP server 是假模型端点，不是 Workspace MCP。

`Sandbox.id / workspaceId` 属于 TS 与工具环境的身份；此命令不改变 Peri Session Store 当前的 `machine_id` 来源。二者要相等仍是独立的 Peri 启动身份契约缺口，不能借 MCP 参数或环境变量假称已解决。

## 验收

- CLI：`peri mcp-start workspace --help`、两种互斥 transport、无效根/参数及缺失工作二进制有确定错误；分派不读取 Peri settings、Store 或模型凭证，不进入 Agent/ACP/TUI 初始化路径。
- 真实进程：检查执行映像与进程树，MCP 请求由独立 `peri-mcp-workspace` 处理；stdio stdout 无日志污染，EOF/信号后后台任务和子进程收口；HTTP 关闭后不再接单，非回环无认证拒绝启动。
- 同一测试夹具分别通过真实 stdio 与真实 HTTP wire 验证七工具的 `tools/list`、代表性 `tools/call`、完整资源清单/读取、订阅、错误映射；两种 transport 的结果一致，不能只调用 handler 单测。
- 后台 Bash 覆盖创建任务后的立即 `tasks/get`、终态结果、按 task ID 订阅通知、取消、断订重订后的查询对账、进程关闭；Peri 侧验证保存 task ID、会话投影与唤醒，而不传本地回调/manager。其他 Agent/Workflow 任务不受迁移影响。
- 资源配置覆盖空根、项目/插件根、关闭 builtin 资产、非法路径；外部 client 可选择不读取或不注入任何资源，Workspace 仍如实提供可见资源。必须验证请求不能越权切换工作区根，并记录 Bash 仍具有当前 OS 用户权限的事实。
- TS SDK 从 Sandbox 启动 HTTP Workspace、等待 MCP 协议握手/发现可用、创建 Agent Session、消费工具/资源、关闭 Agent 后保留 Workspace、最终由 Sandbox 关闭；不要求此 issue 解决多实例接管、Serverless 或 Peri `machine_id` 注入。

实施完成后，按 `DOC-UPDATE-001` 同步 CLI 帮助、MCP 包代码索引、SDK demo 与当前设计文档；在此之前以上命令仅是目标接口。

## 落地切分

1. 将后台 Bash 的执行与任务状态所有权移至 Workspace 能力包；保留 Agent/Workflow 任务在 Peri。剔除 Workspace 对 session manager、session callback 的输入及相关降级，并让当前 builtin 与独立进程共用 Workspace 自有的任务 owner。
2. 实现并对外声明 MCP Tasks 扩展；后台 Bash 的任务创建、查询、取消和通知均走标准 wire。Peri 客户端持有 task ID、订阅映射及会话呈现投影，重连后 `tasks/get` 对账；现有 `subscriptions/listen` 消费分支目前忽略 Task 通知，须补齐，不广播给无关 session。
3. 将资源根解析从 Peri 会话装配改为 Workspace 启动配置和默认目录规则。provider 继续唯一负责扫描与读取；Peri 与其他 client 自行决定资源消费和注入。随后新增独立 CLI/stdio/HTTP 宿主，并用两种 wire 跑同一行为契约。

[^mcp-tasks]: [MCP 2026-07-28 Tasks 扩展](https://tasks.extensions.modelcontextprotocol.io/specification/draft/tasks)规定 `tasks/get`、`tasks/cancel`、经 `subscriptions/listen` 的 `notifications/tasks`，且不提供 `tasks/list`；[2026-07-28 规范发布说明](https://blog.modelcontextprotocol.io/posts/2026-07-28/)说明新订阅流替代旧的 HTTP GET 通知通道。
