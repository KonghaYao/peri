# Print 内联设置写入固定临时文件

状态：实现完成；非 E2E 回归通过。真实双进程 print 验收按本轮要求未执行。

日期：2026-10-04。用户已明确要求处理 print；TUI 端文件操作不属于“减少文件系统依赖、推崇网络依赖”的架构扫描范围，本 issue 独立跟踪 print 的并发与敏感配置问题。

## 现状与风险

`peri-tui/src/cli_print.rs:70-83` 处理 `--settings '<JSON>'` 时，将完整 JSON 写到固定的 `${temp_dir}/peri-settings-override.json`，然后通过 `ConfigSource::load_standalone` 读回。文件名对所有 print 进程相同，写入没有独占创建或并发隔离，也没有清理；设置中可能包含 provider key 或 token。

- 两个 print 进程同时使用不同内联设置时，可能互相覆盖，导致实际 provider/config 与各自输入不一致。
- 配置正文可能遗留在临时目录；文件创建权限随平台和进程默认策略，当前代码没有明确限定。

## 期望处理与验收

- 内联 JSON 从内存注入配置装配，不经过临时文件；保持 `--settings <path>` 的单文件来源语义，以及内联 JSON 不合并全局/工作区设置的现行语义。
- 用两个不同的内联设置并发运行 print，验证各自使用自身设置；验证执行后没有新建配置临时文件或遗留敏感内容。
- 核对配置解析失败及 provider 初始化失败时也不会写入设置正文。

## 实施与验证（2026-10-04）

`ConfigSource::load_standalone_inline_at` 将内联设置作为只读内存来源装配，不读取 global、workspace 或 project 配置文件；print 保留 `--settings <path>` 的单文件来源分支。删除了固定临时文件写入。

- `peri-config` settings 单测 29/29，通过并发不同内联设置、无效外部配置文件隔离和失败时零新文件检查。
- `peri-tui` bin 单测 91/91，通过 print 实际配置分流入口的并发隔离、JSON 解析失败及 provider 初始化失败检查。
- `fmt --all --check` 通过。真实双进程 print E2E 未执行。

相关架构扫描见[文件系统边界残留 issue](2026-10-04-filesystem-boundary-residual-scan.md)。本项按用户裁决独立处理，不计入该扫描的工作区文件系统越界数。
