# WASM 分支可下沉主项目的改动建议

状态：基于 `pre-release/main` 的 `3a999a1e` 重新核对。主线已接收 Resources 迁移规划、MCP task scope、Tokio 基础配置与按需精简；本分支保留时间重构、Resources 共用规则和 WASM 实现。以下“已完成”只表示主线代码已包含相应改动；合并后的验证结果见文末。

## 判断原则

主项目接收 Rust 核心中由多个部署共同消费的领域规则、协议契约和能力接口。`@peri-sdk` 整体留在当前分支，不列入本轮下沉范围。Emscripten 与 Workers 的具体实现留在使用它们的部署层。下沉后应能删掉分支上相应的重复规则或适配补丁；若仍需维护两份规则，说明边界还没有收敛。

主线已经接收 `HostCapabilities`、远端会话执行端口工厂、虚拟环境入口、MCP 能力准入和 ACP `WireBridge`，见[上轮反馈与验收](2026-10-04-wasm-mainline-feedback.md)。这些不是本轮重新提出的工作。

## 合并后状态

| 原建议 | 主线进度 | 合并后处理 |
| --- | --- | --- |
| Resources 共用会话规则 | 已与 WASM 虚拟执行收敛为 `1a1bea3b`；`pre-release/main` 尚未合并。 | 主项目若只接收共用规则，需从该提交按路径与契约另行拆分；本分支不再携带重复规则。 |
| 远端草稿初始化准入修复 | 已纳入 `1a1bea3b`；`pre-release/main` 尚未合并。 | 与同提交的 Resources 改动一起评审；离线准入回归与本机 Resources 测试通过，真实云写入仍需显式环境验证。 |
| 远端 Workspace task scope | 主线已通过 `018459f7` 接收共用类型归位；主线既有的可信 MCP 连接准入保持不变。 | 本分支不再重放该提交；本地 scope/执行代际与 rewind 定向测试通过，远端托管连接回环仍需单独验收。 |
| 日期格式化与有界等待 | 已从 WASM 改动拆成 `e78cfe82`；主线 `pre-release/main` 尚未合并该提交。 | 主项目可独立合并；WASM 分支继续携带该提交。原生检查及远端恢复/关闭、冻结快照定向测试通过；本次重放后 Emscripten 目标检查通过。 |
| Cargo Tokio feature 边界 | 基础配置已由主线 `02e97e52` 接收；按需精简已由主线 `3a999a1e` 接收，本分支不再重放。 | workspace 只声明共用 runtime 能力；需要原生专属能力的 crate 才在目标依赖中启用 `full`，仅测试用 TCP 的 crate 在原生测试依赖中启用 `net`。不包含 Emscripten 构建或依赖版本锁定；独立树 `--locked --workspace` 原生检查通过。 |
| Resources 存储 v2 迁移规划 | 主线已通过 `8481595c` 接收；本分支不再重放该提交。 | 本机和远端升级从 `sessions::storage_v2_plan` 引用同一规划。规划测试 5 项和远端 v11→v12 回归 1 项通过。 |
| Resources 虚拟执行、配置来源、轻量核心依赖 | 尚未由本轮主线提交完成；主线已有的虚拟环境入口属于上轮成果。 | 按下文契约逐项评审。 |
| `@peri-sdk` | 按用户决定，整体保留在此分支；主线这次对 SDK Sandbox 的路径和密钥处理有更新，合并时已适配当前 SDK 接口。 | 不列入下沉范围。 |

## 建议先下沉的工作

| 工作项 | 当前分支的证据 | 主项目应持有的内容 | 验收与分支回收 |
| --- | --- | --- | --- |
| Resources 共用会话规则 | `peri-resources/src/sessions/{canonical,failure}.rs`，`sqlite_store/{row_mapping,failure}.rs`，`remote/schema.rs` | 把消息角色、标题提取、领域失败映射和 schema 13 版本放在本机与远端 adapter 都可引用的位置；保持错误种类和历史数据含义一致。 | 本机和远端的序列化、标题、错误及 schema 升级契约均通过；合并后删除分支为绕过 `sqlite_store` 依赖而复制的规则。 |
| 远端草稿初始化准入修复 | `peri-resources/src/sessions/remote/session_write.rs` 的 `begin_initialization` 无 owner token 准入 | 在原有草稿创建流程内修复远端写入守卫；仅允许协议规定的初始化步骤，不扩大其他无 owner 写入。 | 新建远端会话可完成草稿初始化；未持有 owner 的普通写入仍被拒绝；加一条真实远端批处理链路的回归。 |
| Workspace task scope 共用类型归位 | `mcp-packages/common/src/task_scope.rs`、`mcp-packages/workspace/src/lib.rs`、`peri-middlewares/src/mcp/client/subscription_tasks.rs` | 将本地 Workspace 与远端客户端共用的 scope 编码、解析及执行代际类型放在 MCP common。主线已采用可信连接作为远端准入边界；共用层只处理 scope 元数据，连接的信任由部署保证。 | 本地 builtin 与受信远端 Workspace 的 scope、`epoch + nonce` fence、重连和卸载关闭回归通过；受信连接之外不能通过可伪造的元数据取得工具执行权。合并后分支无需再携带类型搬迁。 |

上表第三项不能只移动文件：主线已明确远端 Workspace 的信任来自部署选定的 MCP 连接，`taskScopeSecretFile`、共享密钥和签名交换均已撤销。合并时应核对主线受信连接准入、Store 执行代际及外部工具 fencing，不恢复旧密钥协议。

## 可以下沉，但需要先定契约

| 主题 | 建议的主线边界 | 当前分支的处理与前置条件 |
| --- | --- | --- |
| Resources 虚拟工作区身份与执行观测 | 由 Resources 定义单一的远端数据、机器身份、工作区发现、执行准入契约；部署注入稳定身份。复用现有 `RemoteWorkspaceEnvironment::Virtual` 与 `RemoteExecution`，让真实目录和虚拟目录各自提供观测。 | 已决定采用主线 `RemoteExecution` 的身份及快照格式，删除分支专用 `WasmExecution`。旧 WASM 会话按 ID 只读历史，旧快照不取得执行资格；新会话使用主线虚拟环境格式。 |
| 配置读取与无本地进程能力 | 共用配置消费者可接受部署提供的配置来源，并明确写入原子性、并发更新和持久性要求；缺席的本地进程、文件锁或 builtin 以能力输入表达。 | `mcp-packages/config/src/wasm.rs` 目前使用 Emscripten 虚拟文件系统和进程内锁；该锁不等于跨实例 CAS。主线需先明确是否允许配置写入及由谁持有持久化，再抽取配置端口。`peri-middlewares`/`peri-acp` 的大量 `cfg(target_os)` 和空实现先保留为平台接线，避免将“不支持”误当成执行成功。 |
| 轻量核心依赖 | Tokio 基础 feature 与按需原生 feature 精简已由主线 `02e97e52`、`3a999a1e` 接收；进程、SQLx、内置 MCP、Workflow、JS runtime 的依赖隔离仍需与调用源码的 `cfg` 和能力入口成套迁移，才可让 Agent 与协议核心在受限目标上独立编译。 | 当前其余 Cargo 调整与源码裁剪仍留在 WASM 分支。`reqwest =0.13.4` 精确锁定、Emscripten 测试依赖及 `peri-wasm` manifest 依赖目标工具链和部署，暂不作为通用配置提交。后续逐项验证原生与受限目标并删除分支对应补丁。 |

虚拟工作区格式已裁决：新会话使用主线 `peri.remote.workspace.v2` 域与 UUIDv8，快照含 `kind: virtual-v1`。旧 WASM 会话使用 `peri-wasm-workspace-v1` 域与 UUIDv5、仅含 `root` 的快照；保留按 ID 读取历史，但不迁移执行资格。相关约束见 [ARC-REMOTE-ENV-001](../../docs/standards/architecture-contracts.md) 和 [Resources 代码索引](../../docs/code-index/peri-resources.md)。

## 留在 WASM 部署或另立议题的部分

| 部分 | 归属理由 |
| --- | --- |
| `peri-wasm/`、Emscripten/Hyper 补丁、`scripts/{cargo-wasm,prepare-emscripten,smoke-wasm-*}`、Workers 示例 | 它们交付特定目标的编译、端口和部署验证；主线共享契约验收完成后，再随 WASM 功能整体合入。 |
| `Resources::open_turso_writable`、`assemble_wasm_server_config` | 这些提供 WASM 部署的凭证、稳定身份和虚拟根输入；共用身份与执行规则由 `RemoteExecution` 持有。 |
| Workflow、image reader、MCP child process、hook 的 WASM 空实现 | 当前只表达目标不可用。主线能力列表和实际工具发现应准确关闭它们；未来若要提供等价能力，按完整行为另立议题。特别是 command hook 的 `Allow` 回退需要审查是否符合 hook 策略，不应作为通用默认。 |
| 整个 `npm-packages/@peri-sdk/`，包括 JSON-RPC 共用传输、stdio/WASM transport、加载器、Yjs `state/`、`view/`、session docs 和交互响应器 | 按当前决策全部留在此分支维护和验收；主项目本轮不接收 SDK 文件，也不以 SDK 改造作为 Rust 核心下沉的前置条件。 |

## 建议实施顺序

1. 主项目已合并 Resources 迁移规划 `8481595c`、MCP task scope `018459f7`、Cargo Tokio 基础配置 `02e97e52` 和按需精简 `3a999a1e`。后续可独立合并本分支的时间 `e78cfe82`；Resources 规则与 WASM 虚拟执行现同属 `1a1bea3b`，只接收共用规则时需再拆分。
2. 在主线可信连接契约下验证远端 Workspace 的代际、重连与关闭，不恢复共享密钥文件。
3. 虚拟工作区持久格式已定；继续逐项收敛配置能力、其余 Resources 与轻量依赖。每次主线合并后删除分支重复规则，并跑原生回归与 WASM ACP、远端 MCP、冷恢复回环。SDK 验证仍在当前分支完成。

验收结果应记录在各自实施议题或测试报告。本文件只提供移交边界；本地 `workerd` 与 Node 回环不能代表托管 Workers 已验收。

本次合并验证：原生 `peri-acp`、`peri-middlewares`、`peri-resources` 的 `cargo check --locked` 通过；`peri-wasm` 的 Emscripten 目标 `cargo check --locked` 通过；MCP common 的 task scope 单测、Workspace 文件恢复 5 项、ACP rewind 57 项通过；SDK `typecheck`、Sandbox 与 WASM transport 共 6 项定向测试通过。Workspace crate 以 `task_scope` 过滤运行时无匹配用例，因此该命令不作为 Workspace 行为验收。合并后尚未重跑完整 Node ACP、远端 MCP 冷恢复及 Workers 回环。

本次重放只改变提交基点，代码树与上次验证时相同；上次原生根 workspace 的 `cargo check --locked --workspace`、`peri-wasm` 的 Emscripten 目标 `cargo check --locked`、SDK `typecheck` 均通过。本次未重跑 Node ACP 与远端 MCP 的端到端回环。
