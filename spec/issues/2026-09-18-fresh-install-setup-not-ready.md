# 全新安装后 setup 未接通运行时，设备初始化与配置恢复存在缺口

**状态**：待平台验收（F01–F09 已修复，macOS 回归通过）
**优先级**：高（首次使用主路径不可用）
**创建日期**：2026-09-18
**类型**：Bug / 首次使用审计
**范围**：首次使用修复已实施；保留此 issue 跟踪 Windows 运行时验收。下方“确认的问题”描述修复前证据，当前实现与验证见文末。

## 问题描述

用户要求检查“系统没有任何 Peri 文件”时的行为，并用 Luna 子代理扩大搜索。初始疑点是缺失目录导致初始化失败，以及 setup 保存后内存 settings 未刷新。

三个 Luna 子代理分别检查持久化目录、setup 与运行时交接、安装资源及外部依赖；主 agent 对关键结论做真实终端和公共 Rust API 验证。默认 TUI 能从空 HOME 创建日志和数据库并进入向导；但保存后不会刷新内存配置，也不会补建之前缺失的 ACP deployment/client/输入消费者。另一个独立入口 `peri sync device init` 确实缺少父目录初始化。

## 复现环境与证据范围

- macOS，仓库当前工作树；`cargo build -p peri-tui --bin peri` 成功，exit 0。链接器有既有的 `__eh_frame section too large` warning。
- 主进程以 `env -i`、独立 HOME/XDG、空工作目录启动；首次运行前没有 `.peri`、`.claude`、配置或缓存。使用独立 tmux socket，不复用用户终端环境。
- 模型请求仅访问 `127.0.0.1:18768` 的 Anthropic SSE fixture；凭据是无效测试字符串。设备测试显式使用临时文件密钥库，没有访问系统 keyring。
- 主 agent 原始证据目录：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/peri-fresh-audit-ufifz3jy`。它是临时证据，不作为长期依赖；下文保留了关键输入、结果和源码入口。
- 公共 API probe 链接本次构建的真实 `peri_tui` rlib；它验证特定函数和生产配置源装配，不等同于完整 TUI E2E。首次 probe 未设置 active alias、另一次未装配 ConfigSource，均已纠正，结论取最终 `17-public-api-probes-with-source.txt`。

## 确认的问题

### F01 · P1 · 首次 setup 完成后当前进程仍不可用

**终端实测。** 空 HOME 启动 → 手动配置 Anthropic、回环地址和测试 key → Done 按 Enter。

磁盘 `settings.json` 已保存正确 provider/address，但主界面没有模型。输入 `FRESH_AUDIT_BEFORE_RESTART` 后编辑框清空，没有会话或模型请求。SQLite `threads` 数量为 0；本地 fixture 请求数为 0。退出并用同一配置重启，再输入 `FRESH_AUDIT_AFTER_RESTART`，收到 `FRESH_AUDIT_LOCAL_RESPONSE`；数据库有 1 个会话，fixture 收到 1 次 `/v1/messages` 请求。

调用链：

1. `peri-tui/src/launch.rs:137` 先解析 provider；仅 `Some` 时创建 host/client，缺失时返回 `None`。
2. `peri-tui/src/kit/entry.rs:178` 在上述装配结束后才显示 wizard。
3. `entry.rs:284` 仅在存在 ACP client 时初始化 `SUBMIT_TX`、`STEER_TX`、notifier、bridge、session 与提交消费者。
4. `peri-tui/src/kit/setup_wizard/handler.rs:294` 丢弃 `save_setup` 的成功返回值，只关闭向导。
5. `peri-tui/src/app/setup_wizard/mod.rs:451` 只合并并保存配置；已有 `App::refresh_after_setup`（`app/mod.rs:185`）没有调用方。
6. `peri-tui/src/kit/input_area/submit.rs:40` 在提交发送者不存在时静默跳过；本地气泡发送者同样未初始化。

**修复边界**：内存配置刷新和 ACP/消费者创建属于同一个首次就绪契约，不能只补一次 settings reload 就宣称恢复。UI 必须在实际可提交后才呈现完成；未就绪时也不能无反馈地清空输入。

证据：`01-first-start.txt`、`04-after-save.txt`、`06-typed-before-restart.txt`、`07-submitted-before-restart.txt`、`09-response-after-restart.txt`、`model-requests.jsonl`。

### F02 · P1 · 保存失败仍关闭向导

**终端实测。** 使用 `--config-file <tmp>/save-target/settings.json` 启动空配置，进入 Done 后，把尚不存在的 `save-target` 建成普通文件，再按 Enter。

配置未写入，日志记录 `setup wizard: save failed: File exists (os error 17)`，但向导消失并进入无模型主界面。`handler.rs:294-297` 的错误分支只有日志，随后无条件关闭。Done 在真正保存之前已经显示 `Setup Complete ✓`，加重了成功误导。

应在界面保留失败状态、输入和重试入口，成功落盘及运行时就绪后再关闭。证据：`11-savefail-before.txt`、`12-savefail-after.txt` 及隔离日志。

### F03 · P1 · 相同 provider ID 的修正被静默忽略

**终端实测。** 已有 `anthropic` provider 指向 `127.0.0.1:18768`，重新 `/setup` 输入相同 ID、新测试 key 和 `127.0.0.1:18769`。保存返回主界面后，整个 provider 对象与保存前完全相同，地址仍为 18768。

`setup_wizard/mod.rs:456-464` 只插入不存在的 ID，已有 ID 不更新 key、URL 或模型。若初始配置含空/错误 key，用户通过向导修正后仍不能恢复，重启也无效。这个问题独立于 F01 的内存交接。证据：`13-same-id-before-save.txt`、`14-same-id-result.json`。

### F04 · P1 · 切换 provider 类型留下不相容的默认字段

**终端实测。** 新建默认 Anthropic provider，在 Type 行按 Right。界面显示 `OpenAI Compatible (anthropic)`，但地址仍为 `https://api.anthropic.com`，四档模型仍为 Claude。

`handler.rs:383-390` 只切换枚举；`MigratedProvider::refresh_provider_defaults`（`setup_wizard/mod.rs:166`）没有调用方。用户填写 key 后，这组配置仍可通过表单校验并保存，重启后会以 OpenAI 协议请求 Anthropic 地址。修复时需区分未改动默认值与用户自定义值，避免粗暴覆盖自定义网关。证据：`10-type-switch.txt`。

### F05 · P1 · 设备初始化依赖其他入口事先创建 `.peri`

**真实 CLI + TTY 实测。** 以空 HOME 执行以下命令，交互输入两次测试密码：

```text
peri sync --keystore-path <empty-home>/.peri/sync-keystore device init --name isolated-audit
```

结果 exit 1：`cannot create keystore .../.peri/sync-keystore: No such file or directory (os error 2)`。HOME 内仍无文件。仅创建 `<empty-home>/.peri` 后重跑同一命令，exit 0，生成 `sync-keystore` 和 `sync-identity.json`。

`main.rs:734-743` 的 sync 分支不经过 TUI telemetry/Resources 初始化；`sync/device_cli.rs:129-130` 直接调用 `FileStore::create`，`sync/keystore.rs:246` 直接独占打开文件，均未创建父目录。`device_cli.rs:333-358` 写 identity 临时文件也未创建父目录，因此即使 keystore 指向另一个可写目录，默认 identity 路径仍有缺口。

无 TTY 时会先按设计拒绝读取密码，不能拿这个错误充当目录缺失的复现。OS keyring 路径未实测；仅从代码看，它还存在先写 keyring、后写 identity 失败的部分初始化风险。证据：`18-device-init.txt` / exit 1 与 `20-device-after-mkdir.txt` / exit 0。

### F06 · P1 · 恢复损坏配置时可能直接覆盖原内容

**真实公共 API 实测。** 对包含恢复标记但 JSON 语法损坏的配置，按生产顺序设置路径、加载 `ConfigSource::load_lenient()`、装配 `CONFIG_SOURCE_HANDLE`，再执行 `save_setup`。最终返回成功，原文件的 `preserve-this` 标记丢失。

`setup_wizard/mod.rs:452` 把任意严格加载错误变成 default，再经启动时保存的配置源覆盖原文件。`peri-acp/src/provider/store.rs:149` 的 lenient loader 保留写回位置，`peri-tui/src/config/mod.rs:21` 使用该句柄保存。用户可能因损坏配置自动进入 setup，再失去原文件中仍可人工恢复的内容。应保留损坏原件或明确进入恢复流程；本项是首次配置失败后的恢复场景，不是纯空文件场景。

证据：`17-public-api-probes-with-source.txt`：`malformed_save_ok=true`、`malformed_original_marker_retained=false`。未将未知字段本身等同于解析错误。

### F07 · P2 · 向导就绪判定与实际 provider 解析不一致

**真实公共 API 实测。** 无任何 provider 环境变量，固定合法 active alias `opus`：

| 配置 | `needs_setup` | `LlmProvider::from_config` |
| --- | --- | --- |
| 有有效 provider，profile.provider 留空 | false | Some |
| active profile 指向不存在的 provider ID | false | None |
| active profile 指向有效 provider，另有未用的空 key provider | true | Some |

`needs_setup`（`setup_wizard/mod.rs:354`）遍历所有 provider 的 ID/key，没有核验 active profile；实际解析在 `peri-acp/src/provider/mod.rs:117,314`。结果既可能跳过必要引导，进入离线空壳，也可能为未使用的备用 provider 强制引导。

**排除反例**：仅 profile.provider 为空不会失败，实际代码会回退第一个 provider；不可把“没写绑定”泛化成 bug。证据：`17-public-api-probes-with-source.txt`。

### F08 · P2 · 连通性检查不验证 HTTPS，并同步阻塞交互

**公共 API 实测 + 调用链确认。** 本地纯 HTTP fixture 对 GET 返回 501；`test_connectivity("http://127.0.0.1:18768")` 返回 true。把同一纯 HTTP 服务地址写为 `https://127.0.0.1:18768`，依然返回 true。

`setup_wizard/mod.rs:653-674` 对所有 scheme 使用裸 TCP、发送明文 HTTP GET，只读取一个字节，不做 TLS 或 HTTP status 检查。因而它既不能证明 HTTPS API 可用，也可能对真实 TLS 端点产生误判。API key 从未参与检查；普通网络可达性检查不必验证鉴权，但名称和结果必须准确表达检查了什么。

`handler.rs:396` 在 UI 事件回调同步调用它。连接和读取各设 5 秒超时，DNS 没有这里统一控制的截止时间，写入也未设超时；不能声称总时长严格上限为 10 秒。可能阻塞键盘、取消和重绘。UI 冻结时长未做计时实测；协议误判已复现。证据：`17-public-api-probes-with-source.txt`。

### F09 · P2 · 安装检查把 Bun-only 环境当作 Workflow 已就绪

**命令存在性模拟 + 当前二进制实测。** 隔离 PATH 仅放一个 `bunx` 占位可执行文件，无 node/npm/npx。安装脚本相同的 `command -v npx || command -v bunx` 检查返回 0；`peri workflow --help` 返回 exit 1：`failed to run node: No such file or directory`。占位 bunx 未被调用；本实验验证检查与执行依赖不一致，不声称测试了真实 Bun 兼容性。

`scripts/install.sh:291-296`、`scripts/install.ps1:259-266` 提示用户可安装 Node.js 或 Bun，并宣称自动通过 npx/bunx 下载；实际 CLI `peri-workflow/src/cli.rs:21` 固定执行 node，Agent workflow 的 `runner/artifact.rs:102` 也固定 node。安装指引会把新用户带到错误的依赖组合。

证据：`19-bun-presence-no-node.txt`。CLI 在临时目录展开内嵌脚本，退出后临时目录被清理；不能把 HOME 下没有 artifact 当作展开失败。

## 条件风险与未验证平台

- **Windows 无 HOME 的 Agent Workflow**：`peri-workflow/src/runner/artifact.rs:47` 只读取 `HOME`，缺失时 embedded 发布失败；其他 Peri 路径通过 `dirs_next` 解析 home。仅设置 USERPROFILE 的新 Windows 环境可能在首次 Agent Workflow 失败。未在 Windows 实测；`peri workflow` CLI 使用临时目录，是另一条路径，不能一并归入此结论。
- **首次日志轮转噪声**：Luna 在空 HOME ACP 启动时观测到 `Error reading the log directory/files: No such file or directory`，随后日志目录创建且启动成功。与 tracing-appender 轮转目录扫描时序一致；本次不计为启动阻塞，未测发生频率。
- `/setup` 的打开动作只设置 `WIZARD_ACTIVE`，未从当前 config 重建表单；再次打开可能是默认表单，也可能保留本进程上一次向导状态。它加重 F03 的恢复体验，但不另算同级故障。
- 先前已有的工作区身份/慢首次提交问题继续由 `2026-09-17-p0-workspace-validation-blocks-input.md` 与 `2026-09-17-platform-compatibility.md` 跟踪，本次不重复计数。

## 正常行为与首次使用依赖

| 条件 | 本次结论 |
| --- | --- |
| 默认 `.peri`、logs、threads 目录均缺失 | TUI 实测能创建并进入 setup；不是默认启动阻塞 |
| `--db-path threads.db` 裸相对路径 | Luna 用当前二进制、隔离 HOME/cwd 实测 ACP initialize 成功，第二次启动也成功；Rust `create_dir_all("")` 返回 Ok，撤销初始误报 |
| schema-lock / WAL 留在磁盘 | 第二次启动成功；残留本身不能证明损坏或无法恢复 |
| 输入历史、用户 skills、插件目录、MCP 配置缺失 | 按默认或空集合降级；内置 skills 通过 `include_str!` 打包 |
| 无 rg | Grep/Glob 使用 Rust 实现，不依赖系统 ripgrep |
| 普通非 Git 空目录 | 本次重启后首条模型请求成功，不是默认建会话阻塞 |
| 无 provider 的 print / ACP | 代码路径清晰报错；没有交互向导属于对应入口的正常行为 |
| 缺失自定义 config-file 父目录 | 正常 save_to 会创建父目录；F02 注入的是不可作为目录的文件 |
| 全局 / workspace / `--config-file` 写回 | 正常 ConfigSource 遵循加载时选定的层，未发现空 HOME 下路径重定向丢失 |
| Agent Workflow 缺失缓存 | 默认先展开内嵌 artifact，需 node；默认不依赖 npm 或网络。npm/npx 仅属于显式 fallback |
| PTC 首次使用 | 需 node；缓存缺失时需 npm 从固定公共 registry 安装固定版本。缺少依赖/下载失败会返回 ArtifactUnavailable，不阻塞普通启动；网络安装未实测 |
| Bash / LSP / 自定义 MCP | Bash 首次执行需平台 shell；LSP/MCP 需各自配置命令。缺失按相关功能错误处理，不泛化成整个 Peri 启动失败 |

## 验证与后续验收

- 主 agent 构建：`cargo build -p peri-tui --bin peri`，exit 0。
- Luna：`cargo test -p peri-tui --lib setup_wizard -- --nocapture`，12 passed。
- Luna：`cargo test -p peri-acp --lib provider::store -- --nocapture`，13 passed。
- Luna：缺失默认数据库的只读打开测试通过，临时 HOME 未创建文件。
- 以上局部单测通过不等于首次使用主链路通过；本次真实终端已暴露 F01–F05。
- 未运行全 workspace 测试、外部模型请求、真实 npm 下载、Linux/Windows runtime 矩阵；未声称这些通过。

上述内容为修复前审计。首次配置的真实生命周期回归现已加入 L0，实施与验收结果如下。

## 状态变更记录

| 日期 | 原状态 | 新状态 | 操作人 | 说明 |
| --- | --- | --- | --- | --- |
| 2026-09-18 | — | Open | agent | 三个 Luna 并行审查，主 agent 隔离复现并去除假阳性；只记录，未修复 |
| 2026-09-18 | Open | 待平台验收 | agent | F01–F09 修复与 macOS 回归通过；用户授权提交，Windows 运行时验收待补 |

## 修复记录

2026-09-18：Luna 完成第一轮实施，Astra 执行第二轮修复与独立代码复审；主 agent 完成真实终端回归；修复、回归测试和剩余平台验收记录一并提交。

- F01/F02：首次向导先完成保存，再在同一 App 上装配唯一 ACP；snapshot 与 consumers 等待 ACP，取消沿 teardown 退出。运行中 `/setup` 等待 ACP 激活成功；保存失败留在向导，落盘后激活失败明确区分。
- F03/F04/F07：按实际 provider resolver 判断是否需要 setup；同 ID 更新保留元数据，重开从当前配置恢复，保留现有 profile/effort。已有 provider 身份保持稳定，类型切换更新协议、默认地址与模型；已有 ID 在向导中只读。
- F05/F06：设备初始化先验证名称与父目录，再写密钥。配置按既定源严格重读并保留最新字段；损坏文件拒绝覆盖。workspace 的全局分层基准被外部修改时拒绝保存并要求重启，避免把过期凭据误写入项目文件。
- F08/F09：连通性检查改为组件拥有的单个异步 HTTP/TLS 请求，统一 5 秒超时，输入变化或卸载取消；结果按 HTTP 状态描述，不表示 key/模型调用已通过。安装检查明确要求 Node.js；Workflow 路径保留显式 HOME override，再回退平台 home。日志目录在轮转扫描前创建。
- 二次复审收口：已有会话按同 ConfigSource owner 更新连接，保留会话选择与 frozen；完整 provider 缓存指纹防旧连接复用。删除会记录 API key 的原始输入事件日志。

### 当前验证

| 命令 / 范围 | 结果 |
| --- | --- |
| `cargo test -p peri-acp --lib provider::store` | exit 0；16 passed |
| `cargo test -p peri-acp --lib -- update_config` | exit 0；6 passed |
| `cargo test -p peri-acp --lib -- session::agent_pool` | exit 0；16 passed |
| `cargo test -p peri-tui --lib setup_wizard -- --nocapture` | exit 0；24 passed；Clippy 修正后相关连通性 3 项再次通过 |
| `cargo test -p peri-tui --lib -- sync::device_cli` | exit 0；12 passed |
| `cargo test -p peri-tui --lib launch::tests::attach_acp_rejects_second_attachment_before_spawning_host -- --exact` | exit 0；1 passed，隔离子进程 |
| `cargo test -p peri-workflow --lib -- tool::tests::test_invoke` | exit 0；2 passed，临时 HOME 下真实 Node 生命周期 |
| `cargo test -p peri-acp --doc`、`cargo test -p peri-tui --doc` | exit 0；各 0 runnable tests，不作为行为证明 |
| `cargo clippy -p peri-acp -p peri-tui -p peri-workflow -p peri-agent --all-targets -- -D warnings` | exit 0 |
| `bash -n scripts/install.sh` | exit 0 |
| E2E：`npm run e2e -- --file tests/scenarios/fresh-setup.test.ts --serial --retry 0` | exit 0；4 passed，无 retry；每次先构建当前 binary |

最终 E2E 证据：`e2e/results/run-2026-09-18T02-53-44/{summary,worker-0}.json`。四项覆盖空 HOME 保存后立即发消息并重新配置同 ID、保存失败修复后重试、Esc 退出、按现有双 Ctrl+C 协议退出。日志非空且不含粘贴的测试 key。模型请求只使用本地 SSE，无真实凭据或外部 API。首次 E2E 已在旧 binary 上复现保存后不可用与保存失败假成功。

按 DOC-UPDATE-001 更新对应 code-index、TUI/ACP 配置契约说明、E2E canonical 路由及 L0。

### 剩余验收

- Windows runner 不可用：PowerShell 安装脚本、没有 HOME 时的平台 home fallback 及 Windows 首次 TUI 生命周期尚未实际运行，不能以 macOS 或静态检查代替。
- Linux 未跑独立矩阵；全 workspace/E2E release 门禁、真实外部模型与 npm 下载不属于本轮验证证据。
- 设备初始化现在保证父目录准备失败前不写私钥；后续磁盘故障的跨 keyring/identity 原子事务不在此修复范围，不能宣称完整事务性。
