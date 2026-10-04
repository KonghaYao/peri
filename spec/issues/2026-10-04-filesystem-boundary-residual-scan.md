# 文件系统边界残留问题：增量扫描

状态：五轮增量静态扫描完成。F1/F2/F3/F8/F19 的代码整改已落地，定向测试已通过；真实异机部署验收尚未完成。F4/F7/F10/F11/F13/F18 仍按既有部署裁决跟踪，F14 暂缓，F15/F16 留待 builtin MCP 组合方案。此文中的旧源码行号是扫描时证据，现状以代码索引和当前源码为准。

日期：2026-10-04。基线为[文件系统依赖清单](2026-09-30-filesystem-dependencies-inventory.md)和[架构边界审计](2026-09-30-architecture-boundary-compliance.md)。本 issue 只记录新增或仍未关闭的生产路径；按用户最新裁决，**TUI 端文件操作不属于本目标**，不计入发现或整改。旧清单的存储后端、日志、Plugin MCP 暂缓与本地 JS 执行环境特例裁决继续有效。证据以当前源码为准，优先级表示影响判断，不是已批准实施顺序。

发现编号保留扫描时的原编号，移出 TUI 项后不连续，避免误把历史引用指向另一问题。

## 判断口径

工作区内容、工作区 Git 状态、工具执行与其产物应由当前会话绑定的工具环境提供；计算实例或 ACP 宿主不能凭相同的 cwd 字符串读取本机文件并当作远端工作区事实。MCP Workspace 内的文件 I/O 属能力实现。配置、存储、日志及本地客户端状态按其实际所有权分别判断，不以 `std::fs` 字样机械定级。

## 本轮整改与剩余验收

| 编号 | 当前实现 | 定向验收 | 剩余 |
| --- | --- | --- | --- |
| F1 | `@path` 经会话可见 Workspace MCP `workspace/readMention` 读取；结果历史记录真实请求名，关闭或断连不回退宿主读盘 | 异机同名内容模拟、行范围、截断、目录上限、路径逃逸及断连 | 真实异机部署 |
| F2 | ACP 只编排历史，`workspace/rewindFiles` 在受信 Workspace owner 内预检并回退；失败不裁剪历史 | 异机同名内容模拟、外部修改、多文件预检失败、Git tracked 恢复、scope/fence wire | 真实异机与应用阶段 I/O 部分失败；该阶段可能已有部分文件变更，错误会显式报告 |
| F3 | Full Compact 删除 Read/Skill 路径回读，仅使用已提交历史 | 同名本机与历史结果冲突、Full Compact 生命周期 | 真实异机部署 |
| F8 | SDK 的外部 Workspace 会话路径保持传入身份，本地 ACP stdio cwd 独立 | 本机 symlink、宿主不存在的远端路径、SDK typecheck | 真实外部 Workspace 部署 |
| F19 | 资源策略只从选中的 ConfigSource 投影；lenient 无快照时使用该来源已读取的全局正文，缺失则关闭 bundled skills | 自定义配置路径与无可信来源、config lib | ACP 真实部署组合 |

以上整改尚未提交。本 issue 继续跟踪剩余部署验收，不以单机定向测试代替跨机结论。

## 2026-10-04 用户裁决

- **F14 LSP：先不处理。** 保留扫描证据，暂不列入近期整改；不据此宣称远端场景已经可用。
- **F15 WebFetch 与 F16 Artifact：纳入未来 builtin MCP 组合方案。** 目标允许将多个 builtin 能力组合为一个较大的 MCP 实例，共享同一个 Workspace 文件视图，以解决 Web 的完整输出与 Artifact 的工作区文件访问。实施时须确保两者实际读写的是会话绑定的 Workspace，不能仅把 handler 放在同一进程却仍使用计算宿主的 cwd/temp 目录；现阶段仅记录方向，不在本次单独修复 F15/F16。
- **Print：需要处理，但不属于本架构扫描范围。** 固定临时配置文件的问题移至[独立 print issue](2026-10-04-print-settings-temporary-file.md)，保留用户此前的处理要求。

## 扫描时确认的越界链路（以下为整改前证据）

### F1 / P1：`@path` 在 middleware 宿主读盘并伪装为 Read 结果

- 生产链：`peri-middlewares/src/assembly.rs:247-249` 仅传入 cwd → `at_mention/mod.rs:77-94` 的 `before_agent` 调用文件读取 → `at_mention/file_reader.rs:32-44,85-86,129-138` 在宿主 canonicalize、读取正文或列目录 → `at_mention/mod.rs:116-142` 自行写入 `Read` ToolUse/Result。
- 影响：计算实例与 Workspace 不同机时可读错或漏读；当前 Workspace MCP 关闭或断连时仍可能读取计算宿主同名文件。生成的历史又显示为 Read 已执行，掩盖实际来源。
- 整改方向：使用会话绑定的 Workspace 能力读取，并保持文件行范围、截断与目录条目限制。`peri-middlewares/src/workspace_io.rs:150-168` 已有 `workspace/readText` 路径，目录接口需另核对。以不同内容的同名本地/远端文件和能力关闭场景验收。

### F2 / P1：ACP rewind 直接修改宿主工作区路径

- 生产链：`peri-acp/src/session/command/rewind.rs:164-175` 从历史提取变更后启动 blocking 回退；`:321-380` 对 `cwd/path` 直接 `read_to_string`、`write`、`remove_file`，并在宿主运行 `git checkout HEAD -- path`。
- 影响：远端 Workspace 时会作用于 ACP 本机路径；文件副作用没有经过当前工具环境的准入与 fencing。即使本机路径不存在，历史仍可能被裁剪，须检查部分失败语义。
- 整改方向：回退执行归受信 Workspace 能力，ACP 保留历史编排；验收远端同名路径、文件已被外部修改及部分回退失败。

### F3 / P1：Full Compact 仍回读计算实例本机文件

- 生产链：`peri-agent/src/agent/compact_v2/full.rs:419-424,503-563` 从历史 Read/Skill 路径重读本机文件并注入模型。
- 影响：远端 Workspace 下可读错机器同名文件，绕过当前能力面。[旧清单 §3.1](2026-09-30-filesystem-dependencies-inventory.md)和[边界审计 F1](2026-09-30-architecture-boundary-compliance.md)已记录，当前源码仍存在。
- 已有裁决：删除文件回读，保留历史工具调用记录；不增加新的本机读盘回退。

## 已确认的部署耦合与待复现问题

### F4 / P1：Workflow 脚本、Git 与 journal 固定在执行宿主本地文件视图

- `peri-workflow/src/tool.rs:288-298` 直接读取 `scriptPath`；`journal.rs:83-273` 将脚本、journal、state 和 outputs 写入 cwd 下目录；`journal/git.rs` 在本机观察 Git。`peri-middlewares/src/workflow/mod.rs:65` 根据 cwd 创建 journal store。
- 旧清单 §3.5 将 Workflow/PTC 作为本地 JS 执行环境特例，故这不是对既有豁免的否定。但它使工作流不能在独立远端工具环境或无共享盘的替换实例中自然恢复。后续迁移需同时处理脚本读取、Git 证据、journal 及执行所有权。

### F5 / P2：可信 Workspace owner 身份需要 SDK 与 ACP 共享密钥文件

- 历史发现；共享密钥文件交互已在前一轮重构中删除，本轮不再实施此项。以下路径为删除前证据。
- `npm-packages/@peri-sdk/src/sandbox/sandbox.ts:100-116,162-165` 在工作区 `.peri/task-scope.secret` 生成密钥并将**文件路径**交给 ACP；`peri-acp/src/host/requests/owner_catalog.rs:18-55` 在 ACP 宿主重读该文件。
- 这要求两进程共享文件视图；跨机或可替换计算实例部署时不成立。当前只确认路径耦合，未做跨机失败实验。改造需保留受信来源、密钥保护及旧 owner fencing 证据。

### F6 / 待复现：ACP 本地工作区探测可能进入 frozen prompt 与配置

- `peri-acp/src/prompt/mod.rs:16-26,40-45` 在本机 cwd 父链探测 `.git` 并冻结 `is_git_repo`；远端工作区同名目录可产生错误值。
- `peri-acp/src/host/prepared.rs:290-335` 以宿主 canonicalize 判断相同目录，另一 cwd 时用 `ConfigSource::load_at` 在宿主加载配置并发现插件。须核对独立 Config MCP bootstrap 对这些路径的覆盖，区分宿主部署配置与工作区配置。尚无远端行为复现，不宣称所有配置请求均绕过 MCP。

### F7 / P2，既有暂缓：插件发现仍取 ACP/middleware 本机缓存

- `peri-acp/src/host/requests/plugin.rs:505-526` 直扫插件缓存；`peri-middlewares/src/plugin/loader.rs:74,167-194,498-501` 读取本机插件清单和 MCP 配置。
- 旧清单 §3.2 已将 Plugin MCP 与缓存迁移暂缓；保留此项用于后续插件资源边界验收，不把宿主缓存误认为远端工作区状态。

### F8 / P1：外部 HTTP Workspace 的路径被 SDK 本机 realpath 改写

- `npm-packages/@peri-sdk/src/sandbox/sandbox.ts:44-53` 对所有 Sandbox 路径执行 `realpathSync(options.path)`，**包括**传入外部 `workspace` 的情况；本机路径不存在时才保留原值，同名路径或 symlink 存在时会改写远端 root。
- 改写后的 `this.path` 又用于 `getSessions` 的 cwd 查询（`:70-72`）及 ACP 会话请求。默认 `createStdioTransport` 在 `:142-155` 还用该路径作本机子进程 cwd，远端路径不存在会阻止默认 transport 启动。
- 建议将本机 canonicalize 限于本机 managed Workspace；外部 Workspace 使用其受信 root/身份，并使 transport 工作目录与远端会话目录分离。需要以本机同名 symlink 指向另一目录及远端路径本机不存在两种场景复现。

### F10 / 过渡耦合：命令 hook 在计算宿主运行本地 shell

- `peri-middlewares/src/hooks/dispatcher.rs:211,365,401` → `hooks/executor.rs:75-105` 在计算宿主以 `input.cwd` 启动命令，插件 root/data 也是宿主路径。远端 Workspace 时无法保证脚本与工作区文件同机。
- `docs/design/mcp-adaptation-v4-part-1.md:205` 明确将 HookMiddleware/loader/executor/command 执行端口暂留宿主，故此处是已批准的过渡边界，**不判为当前契约违规**。远端部署仍需迁移执行能力或明确拒绝不支持的 hook，并验证超时、取消和结果返回。

### F11 / 部署能力缺口：Config MCP 默认仍在计算进程内读本地配置

- `peri-config/src/source.rs` 已经通过配置 MCP 获取正文；但 `mcp-packages/config/src/lib.rs:14-25` 未安装客户端时默认创建 `ConfigurationClient::local()`。本轮生产源码搜索未找到 `install_client` 或 `connect_tcp` 的调用点。
- 这符合当前“文件 I/O 在 MCP 层”的实现，不是宿主绕过 MCP；但默认装配仍要求计算实例有宿主配置文件，不能据此宣称配置服务已网络独立驻留。需在首次配置读取前注入外部可信配置 MCP，并做无共享文件视图的部署测试。

### F13 / 待明确部署契约：stdio MCP server 按计算宿主 cwd 启动

- 静态 MCP 的 `peri-middlewares/src/mcp/client/process.rs:107-118` 对子进程设置 `current_dir(cwd)`；动态 stdio 的 `mcp/dynamic/staged_connection.rs:418-432` 默认采用 pool 的 `execution_cwd`，同样在计算宿主 spawn。
- stdio transport 本身表示本地进程，不能简单判为绕过 MCP；但当会话 Workspace 独立远端驻留时，配置中的命令及 cwd 是否属于计算宿主还是工具环境必须明确。当前默认路径会要求计算宿主存在相同目录，且该 server 看到的是计算宿主文件。需用远端工作区 + stdio MCP 配置验证准入/错误提示，不能把其文件访问解释成远端 Workspace 能力。

### F14 / 暂缓：LSP 工具首次 didOpen 从 ACP 宿主读取工作区文件

- `peri-acp/src/host/assemble.rs:443-447` 在宿主创建 LSP pool；`mcp-packages/lsp/src/tool.rs:108-126` 的 `ensure_file_open` 用 `tokio::fs::read_to_string(file_path)` 读取正文再发 didOpen，文件查询在 `:262-277,305-311,343-344` 调用它。
- `peri-middlewares/src/assembly/lsp.rs:29-38` 的 LspSyncMiddleware 已经从会话 Workspace MCP 取正文，但上述**查询首次打开/重启后打开**没有复用该输入。远端 Workspace 下可能跳过 didOpen 或发送 ACP 本机同名文件内容，污染 LSP 结果。用户裁决先不处理；后续恢复时应统一 LSP 文档正文来源，并以异机同名不同内容及 server 重启验证。

### F15 / 未来 builtin MCP 组合：WebFetch 全文保存到 Web 实例临时文件，却提示 Workspace Read

- `mcp-packages/web/src/web_fetch.rs:68-97` 在包内截断内容时调用 `persist_truncated_output`；`mcp-packages/common/src/shell.rs:361-387` 将全文写到**当前 Web MCP 进程**的 temp 目录，提示用 Workspace `Read` 读取该路径。
- Web 与 Workspace 分机时提示路径不可达。`peri-middlewares/src/mcp/client/output_store.rs:133-164` 只能处理 Web 已截断后的 MCP 响应，无法再保存原全文。现有 `web_fetch_test.rs` 只验证同进程路径可读。按用户裁决，未来将 builtin 能力组合并共享 Workspace 文件视图；验收须从 Workspace `Read` 取到完整内容，且不能依赖计算宿主临时目录。

### F16 / 未来 builtin MCP 组合：Artifact builtin 在 ACP 宿主读取待上传文件

- `peri-middlewares/src/mcp/builtin/dispatch.rs:258` 用宿主 `ctx.cwd` 创建 Artifact handler；`mcp-packages/artifact/src/tool.rs:41-78,169-178` 对 `file_path` 在该进程 stat、读取并上传。
- 文件 I/O 位于 MCP 包内，包独立部署且与 Workspace 同文件视图时可以合理；**当前 builtin 装配在 ACP 宿主**，远端 Workspace 产物会读不到或读错本机同名文件。按用户裁决，未来的组合实例应与会话 Workspace 共享文件视图；验收容量、扩展名及内容来源，确保读取的是 Workspace 产物。

### F18 / 恢复目标缺口：Workflow CLI 只能从调用者本机目录读取成果

- `npm-packages/@peri-workflow/src/reader.ts:22-50,278-286` 的 read/list 从 `process.cwd()` 向上查 `.claude/workflow-runs`，再读本机 state、outputs 和 journal；`peri-workflow/src/cli.rs:36-54` 允许这些 CLI 子命令。
- 这与旧清单 §3.5 的 Workflow 本地 JS 特例一致，不判为现行契约违规；但计算实例替换或异机客户端不能据 run ID 找到原执行成果，和 F4 的 journal 本地所有权共同阻碍恢复目标。后续应经稳定存储/资源入口按 run ID 查询。

### F19 / P1：配置快照缺失时重新读取默认全局技能关闭位

- `peri-acp/src/host/workspace.rs:258-263` 装配 Workspace 资源时，本应取选中 `config_source.snapshot().resources().disable_bundled_skills`；快照为 `None` 时却调用 `peri_middlewares::skills::load_disable_bundled_skills()`。该函数在 `peri-middlewares/src/settings.rs:16-31` 从**默认全局配置路径**经 Config MCP 重新读取，失败按 false。
- `peri-acp/src/host/stdio/mod.rs:116-145` 在非注入配置的 `load_at` 失败时用 `load_at_lenient`；`peri-config/src/settings.rs:193-233,271-273` 的 lenient 无 authority，因而 `snapshot()` 为 `None`。选中自定义配置路径且发生 lenient 准入时，资源关闭位可能来自另一份默认本机设置，与已加载配置不一致。`peri-acp/CLAUDE.md:28` 明确要求使用快照且不重读全局关闭位。
- 这是配置来源与能力关闭语义漂移，不是直接读技能正文。修复时应从同一份已加载配置/资源投影取得值，或在无可信投影时明确关闭/拒绝；不得回退默认路径。验收自定义配置路径和 lenient 无 authority 两种场景。

### 潜在接口缺口：共享命令参数解析器检查宿主 Path 是否存在

- `peri-acp-types/src/command_args.rs:103-109` 的 `ArgKind::Path` 在纯契约解析阶段调用宿主 `Path::exists()`；该解析器被 Agent/ACP 命令拦截使用，但本轮未找到生产命令声明 `ArgKind::Path`，只有测试使用。
- 当前不列可触发故障。若将来启用 Path 参数，存在性须由工作区能力端判断，不能由协议类型层检查计算宿主文件视图。

## 扫描覆盖与继续核对

五轮增量排重记录（只统计本目标范围内的新发现；TUI 本地文件操作已剔除）：

| 轮次 | 侧重 | 新发现 |
| --- | --- | --- |
| 1 | 直接/间接文件调用，SDK 与 MCP 输出 URI | 无；命中均已记录或属合理能力实现 |
| 2 | 进程 cwd、输入附件、stdio MCP 与 shell | 无；命中为既有过渡边界，客户端输入不计入本目标 |
| 3 | 远端冷恢复、配置快照、builtin 关闭和断连 | F19；远端 Virtual 恢复未见本机工作区读盘 |
| 4 | Agent/Workflow 恢复、其余 MCP 包 | 无目标范围内新增；Workflow journal 仍归既有 F4/F18 |
| 5 | 失败降级时的本机文件回退、会话链反向抽查 | 无新增确认问题 |

- 已按生产源码搜索 Rust workspace 各 crate 的直接文件调用、路径探测及本机进程 cwd；补扫 `mcp-packages` 的 Web、Artifact、LSP、Workspace、Config、Common 等实现及 `npm-packages/@peri-sdk`、`@peri-workflow` 的运行时路径。构建、发布、迁移、站点检查脚本及 `side-projects` 与主会话运行链分开看待。搜索结果再沿上述调用链核对；这仍不是对第三方库内部 I/O、动态命令、所有协议路径或跨机行为的穷尽证明。
- `peri-controller` 与 `peri-runtime` 生产源码未发现直接文件 I/O；本地 SQLite/远端 Store、MCP Workspace 内部读盘与观测日志按旧清单裁决，不列为上述违规。TUI 端文件操作按用户最新裁决整体排除；print 单独跟踪。
- 后续重点：真实异机文件视图、远端会话冷恢复、能力关闭后的 fallback、资源 URI 生命周期；对每项补能区分两台机器内容的复现。静态扫描完成不等于这些验收完成。
