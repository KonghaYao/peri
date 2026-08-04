---
name: ssh-cli
description: Execute commands and manage files on remote machines over SSH via a zero-dependency Node CLI (ssh-cli.js). Use when the user needs to run commands, read/write/edit files, or diagnose connectivity on a remote host, or asks for anything "on the server / remote machine / dev box".
---

# SSH CLI

通过系统 `ssh` 二进制操作远程机器。单文件零依赖 CLI，无需安装任何东西：

```
node .claude/skills/ssh-cli/scripts/ssh-cli.js <子命令> ...
```

## 子命令

| 子命令 | 用途 | 示例 |
| --- | --- | --- |
| `exec` | 远程执行命令（支持管道/&&/重定向） | `exec dev-server "cd /app && npm test"` |
| `read` | 读远程文本文件（可只读行区间） | `read dev-server /etc/hosts --offset 5 --limit 10` |
| `write` | 写/追加远程文件 | `write dev-server /app/.env "KEY=value"`（追加加 `--append`，长内容用 `--stdin`） |
| `edit` | 精确替换（old_string 必须唯一，多匹配加 `--all`） | `edit dev-server /app/main.js "old" "new"` |
| `ls` | 目录列表 | `ls dev-server /app` |
| `push` | 本地上传远程（scp，`--recursive` 传目录） | `push ./dist dev-server /srv/app/dist` |
| `pull` | 远程下载本地（scp） | `pull dev-server /var/log/app.log ./app.log` |
| `test` | 连接诊断（主机名/系统/连通性） | `test dev-server` |

## 参数约定

- `host`：`user@host` 或 `~/.ssh/config` 别名；凭据全部走系统 ssh（config/agent/key），命令行不传密码
- 全局选项：`--hosts <白名单>`（逗号分隔，`*` 放行所有；不传默认放行）、`--timeout <ms>`、`--port <n>`、`--key <path>`、`--audit-log <path>`
- 环境变量：`SSH_CLI_ALLOWED_HOSTS` / `SSH_CLI_TIMEOUT` / `SSH_CLI_PORT` / `SSH_CLI_AUDIT_LOG`
- 退出码：`exec` 远程非零退出返回同码；超时 124；其他错误 1 —— 可用于判断成败

## 工作流

### 1. 先诊断再操作

未知/新主机先 `test`，确认连通性与认证：

```bash
node .claude/skills/ssh-cli/scripts/ssh-cli.js test user@10.0.0.5
```

### 2. 多步操作用 && 链式（无状态语义）

```bash
node .claude/skills/ssh-cli/scripts/ssh-cli.js exec prod "cd /srv/app && git pull && npm ci --omit=dev && pm2 restart app"
```

### 3. 文件编辑走 read → edit 两步（避免一次改错）

```bash
node .claude/skills/ssh-cli/scripts/ssh-cli.js read dev-server /srv/app/config.js
node .claude/skills/ssh-cli/scripts/ssh-cli.js edit dev-server /srv/app/config.js "port: 3000" "port: 8080"
```

### 4. 文件传输用 push / pull（scp 包装）

单文件/目录都支持；大目录建议 `--recursive`，传输超时调大 `--timeout`：

```bash
node .claude/skills/ssh-cli/scripts/ssh-cli.js push ./build/ dev-server /srv/app/build --recursive
node .claude/skills/ssh-cli/scripts/ssh-cli.js pull prod /var/log/nginx/access.log ./access.log
```

需要增量同步/断点续传的大目录时，直接用 rsync（两端都要有）：

```bash
rsync -avz --partial ./dir/ user@host:/srv/dir/
```

### 5. 大文件/长内容

- 读大文件：`read` 只传输需要的行区间（sed 实现），返回前会打印总行数
- 写长内容：`write` 用 `--stdin` 从 stdin 读，避免命令行长度限制与注入风险

```bash
cat local-file.txt | node .claude/skills/ssh-cli/scripts/ssh-cli.js write dev-server /srv/app/data.json --stdin
```

### 6. 安全建议

- 生产主机建议配置 `--hosts "prod"` 白名单，白名单外主机直接拒绝
- 敏感操作（部署、改配置）配合 `--audit-log /tmp/ssh-cli-audit.jsonl`，每次调用留痕（含命令与退出码）

## 已知限制

- 单次输出无上限（stdout 直接透传）；二进制文件会被检测并拒绝（仅文本）
- `multiline` 正则类搜索不支持（无 grep 子命令；需要时用 `exec` + `rg`）
- 无持久会话：`exec` 每次新 shell，跨步骤状态用 `&&` 保持
