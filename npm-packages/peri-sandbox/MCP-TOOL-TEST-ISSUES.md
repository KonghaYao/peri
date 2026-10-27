# peri-sandbox MCP 工具测试问题报告

- 测试时间：2026-10-27 16:30–16:35 CST
- 代码版本：`6813e61`（工作区另有未提交修改：`src/runtime/backend.ts` 等）
- 环境：macOS 26.5.1 (arm64)，Node `v26.9.0`，peri-sandbox 0.1.0
- 沙箱：`sbx_da1a2109ad0173f2780b6df21b3aa3ee`，profile `workspace-default`
- MCP 端点：`http://127.0.0.1:52961/sandboxes/sbx_da1a2109ad0173f2780b6df21b3aa3ee/mcp`
- 测试方式：通过 MCP 逐个调用 7 个工具（`Bash` / `Read` / `Write` / `Edit` / `Glob` / `Grep` / `folder_operations`）

## 结论

7 个工具本身在沙箱 workspace 内功能正常。真正属于 peri-sandbox 服务的问题只有 1 个 P1（ISSUE-2：`/bin`、`/sbin` 元数据被拒），另有 1 个 P3 DX 问题（ISSUE-3）。

原 ISSUE-1 经复核后**不是 MCP 服务缺陷**：服务日志无错误/崩溃痕迹，崩溃报告目录也为空，现象只能说明「服务当时未运行，而客户端仍持有静态端点」。已改列为 OBS-1，属于启动器/部署/宿主连接管理范畴，不要求服务端修复。

| 编号 | 类型 | 严重度 | 问题 | 影响 |
|---|---|---|---|---|
| ISSUE-2 | 服务缺陷 | P1 | 沙箱内 `/bin`、`/sbin` 元数据被拒绝，Bash 裸命令全部报 not found | `ls`/`rm`/`cat` 等基础命令必须写绝对路径，脚本与 Agent 常规写法失效 |
| ISSUE-3 | DX | P3 | 越界写只返回 `Operation not permitted (os error 1)` | 无法从错误信息得知允许写入的 workspace 根目录 |
| OBS-1 | 环境观察 | — | 测试开始时 MCP 端点未运行（无崩溃证据） | 工具全部不可用，跑 `start.sh` 后恢复；由启动器/宿主处理 |

---

## OBS-1（非 MCP 服务缺陷）测试开始时 MCP 端点未运行

> 复核结论：没有证据表明 peri-sandbox 的 MCP 服务实现有问题。现有现象只能说明「服务当时没有被 launchd 托管/没有进程监听」，属于启动器、部署配置或宿主连接管理范畴。

### 现象

首次调用任意 peri-sandbox 工具时全部失败：

```
MCP 服务器 "peri-sandbox" 工具 "Bash" 调用失败:
Transport send error: ... error sending request for url
(http://127.0.0.1:52961/sandboxes/sbx_da1a2109ad0173f2780b6df21b3aa3ee/mcp)
```

### 证据

- `curl -sS -m 3 http://127.0.0.1:52961/...` → `curl_exit=7`（connection refused），`lsof -iTCP:52961 -sTCP:LISTEN` 无输出。
- `launchctl print gui/501/dev.konghayao.peri-sandbox-local` → `Could not find service ... in domain for user gui: 501`。
- `data/control/service.err.log` 为空；`data/control/service.log` 最后一条是 16:30 的正常 `control_request`，日志中没有 `error`/`fail`/`shutdown`/`signal` 记录。
- 当天 `~/Library/Logs/DiagnosticReports` 下没有 node/peri 相关崩溃报告。
- 执行 `./data/start.sh` 后服务恢复，URL/token 不变；随后 7 个工具全部调用成功。

### 为什么不算服务缺陷

- 服务端 `serve()` 是常驻进程（`src/server/service.ts:489-505`），没有空闲自动退出逻辑，不存在「空闲后自行关闭」的实现。
- 若服务崩溃，通常会在 stderr 或崩溃报告中留下痕迹；本次两者都没有。
- 更可能的情况是：服务被外部 `SIGTERM`/`launchctl bootout`/系统重启/人工清理停止，或本次会话开始前服务就未被 bootstrap。客户端 `.mcp.json` 指向静态 URL，因此会继续访问一个已经不存在的端点。
- 因此修复责任不在 MCP 工具实现，而在「谁负责让服务常驻、谁负责在连接失败时恢复」。

### 可选加固（不属于本服务缺陷修复）

如果产品目标是「服务常驻、MCP 端点始终可用」，可以在启动器/部署层做以下任一改进：

1. `data/control/launch-agent.plist:18` 设 `KeepAlive=true`，进程退出后由 launchd 拉起。
2. 将 launch agent 安装到 `~/Library/LaunchAgents`（或提供 `install` 子命令），保证登录后自动加载。
3. 宿主在 MCP 连接失败时自动执行一次 `start.sh`，或提示明确的一键修复命令。
4. 可选：服务端收到 `SIGTERM` 时写一条结构化 `shutdown` 日志，便于区分「外部停止」与「崩溃」。

### 验收标准（仅当产品要求常驻时）

- 对服务进程执行 `kill -TERM <pid>` 后 5 秒内 `launchctl print` 显示 `running`。
- 重新登录/重启后无需手动操作，MCP 端点可用，或工具报错中给出明确的修复提示。

---

## ISSUE-2（P1）沙箱内 `/bin`、`/sbin` 元数据被拒绝，Bash 裸命令全部 not found

### 现象

沙箱 Bash 的 `PATH` 正常（`/Users/konghayao/.hermes/node/bin:/usr/bin:/bin`），`/bin/ls` 存在且可执行，但：

- `ls` → `bash: ls: command not found`，工具层提示 `Command 'ls' not found in PATH`
- `rm` → 同样失败
- `command -v ls` / `type ls` / `/usr/bin/which ls` → 均找不到
- `/bin/ls`、`/bin/rm` 用绝对路径调用则成功

### 复现步骤

在 peri-sandbox MCP 的 `Bash` 工具中执行：

```bash
pwd; echo "PATH=$PATH"; command -v ls; /bin/ls -la .
/usr/bin/stat -f '%Sp %N' /bin/ls
```

观察到的输出：

```
PATH=/Users/konghayao/.hermes/node/bin:/usr/bin:/bin
# command -v ls 无任何输出
stat: /bin/ls: stat: Operation not permitted
```

同时 `/bin/ls -la` 可以正常执行 —— 说明不是文件缺失，也不是 exec 被拦，而是元数据读取被拒绝。

### 根因

- `src/runtime/policy.ts:82-97` 生成的文件系统策略为 `denyRead: ['/']` + `allowRead: [profile.runtimeReadPaths..., workspace, home, tmp]`。
- 默认 profile 的 `runtimeReadPaths`（`src/runtime/mac-config.ts:51`）只包含 `/usr`、`/System`、`/Library`、`/private/etc`、peri 可执行文件，**缺少 `/bin` 和 `/sbin`**。
- macOS 上 `/bin` 是真实目录（不是 `/usr/bin` 的符号链接；且本机没有 `/usr/bin/ls`），因此 `/bin/ls`、`/bin/rm` 不在 allowRead 范围内，`stat` 被 sandbox 拒绝。
- bash 的 PATH 查找依赖 `stat`/`access`；元数据被拒后直接判定命令不存在，于是报 `not found in PATH`。

### 影响

- `ls`、`rm`、`cat`、`cp`、`mv`、`mkdir`、`chmod`、`date`、`sleep`、`unlink` 等大量基础命令必须写绝对路径。
- 脚本、Makefile、Agent 的常规写法（依赖 PATH）全部失败，且错误信息误导为「未安装」。
- 这是沙箱 Bash 可用性的核心路径，建议按 P1 处理。

### 修复建议

1. 在 `src/runtime/mac-config.ts:51` 的默认 `runtimeReadPaths` 中加入 `/bin`、`/sbin`（保留 `existsSync` 过滤）。
2. 同步更新已生成的 `data/control/config.json` → `profiles.workspace-default.runtimeReadPaths`。该配置由 init 生成，不会自动迁移。
3. 在 `tests/integration/mac.ts` 增加回归用例：沙箱内执行 `stat /bin/ls` 与 `ls`，断言成功。
4. 可选：policy 生成时校验 `/bin`、`/sbin`、`/usr` 是否覆盖，缺失时直接报 `unsupported_environment`，避免静默降级成「命令不存在」。

### 验收标准

- 沙箱 Bash 中 `command -v ls` 返回 `/bin/ls`，`ls`、`rm`、`cat` 裸命令可执行。
- `stat /bin/ls` 不再返回 `Operation not permitted`。
- 集成测试覆盖该场景。

---

## ISSUE-3（P3）写 workspace 外路径时错误信息不可诊断

### 现象

通过 `Write` / `folder_operations` 写 `/Users/konghayao/code/ai/peri-sandbox/data/_mcp_test`（沙箱 workspace 之外）返回：

```
Operation not permitted (os error 1)
```

### 分析

沙箱策略只允许写 `workspace`、`home`、`tmp`，拒绝越界写是**预期行为**；问题在于工具层直接透传 OS errno，没有告诉调用方允许写入的根目录是什么。

### 修复建议

- 在 `Write` / `Edit` / `folder_operations` 捕获 `EPERM`/`EACCES`，补充上下文，例如：
  `write denied: /path is outside the sandbox workspace (/.../sandboxes/<id>/workspace)`
- 或在工具返回值/描述中暴露 `workspaceRoot`。
- 仅 DX 改进，无安全影响，优先级 P3。

### 验收标准

- 越界写返回的错误中包含允许写入的 workspace 绝对路径。

---

## 测试矩阵

| 工具 | 测试内容 | 结果 |
|---|---|---|
| `Bash` | `pwd`、`id`、`PATH`、非零退出码 `7`、stderr 输出、绝对路径执行 | ✅ 正常（非零退出码以工具错误呈现，输出保留） |
| `Read` | 全文读取、`offset=2, limit=2` 分段读取 | ✅ 正常 |
| `Write` | 新建文件、`append=true` 追加 | ✅ 正常 |
| `Edit` | 精确替换一行 | ✅ 正常 |
| `Glob` | `_mcp_test/*.txt` | ✅ 正常 |
| `Grep` | `hello` 内容搜索（content 模式） | ✅ 正常 |
| `folder_operations` | `create` / `exists` / `list` / `deep_scan` | ✅ 正常 |

测试文件位于沙箱 workspace 的 `_mcp_test/`，测试结束后已用 `/bin/rm -rf` 清理，workspace 无残留。

## 其他观察（非缺陷）

- `Bash` 的非零退出码会以 MCP tool error 形式返回（如 `[Exit code: 7]`），stdout/stderr 完整保留。若这是有意设计，建议在工具文档中注明，避免 Agent 把正常的非零退出误判为工具故障。
- 只读工具在 workspace 内均正常；本次未测试跨 workspace 读取，按策略应被拒绝。
- 沙箱内 `HOME` 指向 `data/sandboxes/<id>/home`，与 workspace 隔离，符合预期。

## 建议修复顺序

1. **ISSUE-2（P1）**：单行配置补充 + 现有 config 迁移，成本最低、对工具可用性提升最大。
2. **ISSUE-3（P3）**：错误信息增强，可在后续迭代处理。
3. **OBS-1**：不属于服务端修复；由启动器/宿主决定是否做常驻与自愈加固。
