# Artifact 远程存储配置

> 面向使用者：怎么把 `artifact` 工具的上传目标从默认公共服务换到你自己的远程存储。
> 本文是操作参考，不构成仓库设计或工程规则；发生冲突时以代码、`docs/standards/` 与
> `docs/design/` 为准。内置实例的名字与关闭语义另见 [mcp-ecosystem.md](mcp-ecosystem.md) §9.8。

## 1. 这个能力做什么

`artifact` 工具把本地文件上传到**远程存储服务**，返回一个带过期时间的公开链接：

- 只接受 `.html` / `.htm` / `.md` 三种文件；
- `.md` 会在上传前转成带样式的 HTML 页面（表格、任务列表、脚注、删除线、代码块、引用、标题），`.html` / `.htm` 原样上传；
- 链接默认 7 天过期，也可以要求 30 天；
- 适合上传生成的报告、看板、原型页面，把链接丢给同事或贴进 issue。

链接是**公开**的：任何拿到 URL 的人都能访问（你的服务若不做鉴权）。**不要把密钥、凭据、内部数据传上去。**

## 2. 默认行为

不配置任何东西时，上传目标是一个公共服务：

| 项 | 默认值 |
| --- | --- |
| 服务地址 | `https://cloud-artifacts.claude-code-best.win` |
| 凭据 | 内置的共享标识（不是你的个人凭据，也不需要你申请） |

含义：**不配置就等于把文件交给第三方公共服务**。如果内容不能出内网，或者要自己控制留存与访问，就按下一节换成你自己的服务。

## 3. 配置：两个环境变量

| 变量 | 作用 | 自建时 |
| --- | --- | --- |
| `PERI_ARTIFACTS_URL` | 存储服务**基地址**。上传实际打向 `{该值}/upload`（结尾的 `/` 会被自动去掉） | 必填 |
| `PERI_ARTIFACTS_TOKEN` | 以 `Authorization: Bearer <token>` 发送的凭据 | 按你的服务要求；不设置时回退到内置共享标识 |

在**启动 Peri 之前**、同一个 shell 里设置：

```bash
export PERI_ARTIFACTS_URL="https://artifacts.example.internal"
export PERI_ARTIFACTS_TOKEN="<你的服务签发的 token>"
peri
```

生效时机：这两个变量在 Peri 建立 `artifact` 实例时读取一次（即启动新会话时）。**改完要重启 Peri**；会话中途改环境变量不会生效。

凭据纪律：不要把 token 写进仓库、配置模板或截图，也不要贴进 issue；用环境变量或机密管理器注入。Peri 不会把服务地址与 token 打印到工具输出或错误信息里。

## 4. 你的服务要实现什么（对接契约）

最小实现就是一个 `POST /upload` 端点。

**请求**（Peri → 你的服务）：

```http
POST {PERI_ARTIFACTS_URL}/upload
Authorization: Bearer <PERI_ARTIFACTS_TOKEN>
Content-Type: text/html
Accept-Encoding: identity
X-TTL: 7d

<文件内容：UTF-8 文本；.md 已转成 HTML>
```

**响应**：必须返回 JSON。HTTP 状态码不作判据——错误写在 body 里：

```jsonc
// 成功
{ "url": "https://artifacts.example.internal/a/9f3c.html", "expiresAt": "2026-10-03T12:00:00Z" }

// 失败
{ "error": "token 无效" }
```

字段约定：

| 字段 | 要求 |
| --- | --- |
| `url` | 成功时必填，原样展示给用户 |
| `expiresAt` | 可选（也接受 `expires_at`），展示为链接过期时间 |
| `error` | 失败时填。**不要只靠 HTTP 状态码**：有些托管平台永远返回 200，Peri 只看 body |
| `id` | 可以返回，会被忽略 |

其他约定：

- 响应 JSON 要小，并且**不要用 Brotli 压缩**（Peri 已声明 `Accept-Encoding: identity`，就是这个原因）。
- `PERI_ARTIFACTS_URL` 只写到基地址，**不要**自带 `/upload`（否则会变成 `/upload/upload`）。
- `X-TTL` 只是期望留存时长的声明（`7d` 或 `30d`）；实际过期由你的服务决定，用 `expiresAt` 告诉用户。

### 本机验证用的最小桩

```js
// stub.mjs —— 只回链接、不真的存文件，够验证配置链路
// 用法：node stub.mjs
// 另开一个终端：PERI_ARTIFACTS_URL=http://127.0.0.1:8787 peri
import { createServer } from 'node:http'

createServer((req, res) => {
  if (req.method !== 'POST' || req.url !== '/upload') {
    res.writeHead(404).end()
    return
  }
  let body = ''
  req.setEncoding('utf8')
  req.on('data', (chunk) => (body += chunk))
  req.on('end', () => {
    const id = Math.random().toString(36).slice(2, 10)
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(
      JSON.stringify({
        url: `http://127.0.0.1:8787/a/${id}.html`,
        expiresAt: new Date(Date.now() + 7 * 864e5).toISOString(),
      }),
    )
  })
}).listen(8787)
```

也可以不启动 Peri，直接用 curl 验证你的服务是否符合契约：

```bash
curl -sS -X POST "$PERI_ARTIFACTS_URL/upload" \
  -H "Authorization: Bearer $PERI_ARTIFACTS_TOKEN" \
  -H 'Content-Type: text/html' \
  -H 'X-TTL: 7d' \
  --data-binary @page.html
```

## 5. 上传侧的限制与行为

| 项 | 行为 |
| --- | --- |
| 扩展名 | 只接受 `.html` / `.htm` / `.md`（大小写不敏感），其他直接拒绝 |
| 大小 | 单文件上限 **10 MB** |
| 工具参数 | `file_path`（必填；相对路径按当前会话工作目录解析）、`ttl`（`7d` 默认，可传 `30d`） |
| 成功输出 | `Artifact uploaded: <url>`，服务返回过期时间时追加 `Expires: <时间>` |
| 输出格式 | 纯文本 URL。终端里的可点击链接由界面渲染，工具输出本身不含控制序列 |

失败时返回一行文本，前缀标明失败发生在哪一步：

| 前缀 | 含义 |
| --- | --- |
| `File not found` / `Not a file` | 路径不存在，或指向目录 |
| `Only HTML or Markdown files are supported` | 扩展名不在白名单 |
| `File too large` | 超过 10 MB |
| `Upload failed` | 连不上服务：地址写错、DNS、超时、网络不通 |
| `Upload error` | 你的服务在 JSON 里返回了 `error` |
| `Failed to parse response` | 响应不是合法 JSON（会附带 body 片段） |
| `Failed to read response` | 响应体读取失败（例如被压缩编码噎住） |

## 6. 排查

| 现象 | 先查什么 |
| --- | --- |
| `Upload failed` | `PERI_ARTIFACTS_URL` 是否只写到基地址、服务是否在监听、本机能否访问该地址 |
| `Upload error: ...` | 服务返回的错误文本；常见是 token 不匹配、超限、内容被策略拒绝 |
| `Failed to parse response` | 服务返回了 HTML 错误页或空 body；按 §4 返回 JSON，并检查是否压缩了响应 |
| 服务返回 401/403 但 Peri 没报错 | Peri 只看 body；请在 body 里写 `{"error": "..."}` |
| 改了环境变量没变化 | 变量在启动时读取：重启 Peri |
| 上传成功但链接打不开 | `url` 由你的服务给出，Peri 不做代理也不托管内容；确认该地址真的对外可访问 |

## 7. 不需要这个能力时

三条关闭路径（任一即可，效果是模型侧不再有 `mcp__artifact__artifact`，且 tool search 目录、子 agent 继承面、workflow 工具列表同步移除）：

```jsonc
// 1) 会话级策略键：项目 .peri/settings.json 或 ~/.peri/settings.json
{ "config": { "meta_harness": { "ArtifactMiddleware": false } } }
```

```jsonc
// 2) 实例级：项目 .mcp.json
{ "mcpServers": { "artifact": { "disabled": true } } }
```

```jsonc
// 2') 实例级：~/.peri/settings.json（两个候选位置都支持，config.mcpServers 优先）
{ "config": { "mcpServers": { "artifact": { "disabled": true } } } }
```

```bash
# 3) 进程级紧急闸门：一次关掉 web 与 artifact 两个内置实例
export PERI_MCP_BUILTIN=off
```

`{"artifact": {"disabled": true, "system_mcp": true}}` 会被加载期拒绝（禁用与「声明为 system 依赖」互斥）。路径 1 只关 `artifact`；想连 Web 能力一起关，把 `"WebMiddleware": false` 写进同一个 `meta_harness` 对象。

## 8. 边界（诚实说明）

- Peri 只做「上传 + 展示链接」：**存储、过期回收、访问控制、内容审查都由你的服务负责**。
- 默认公共服务的可用性、留存时长与配额不属于本项目的承诺；要可控就自建。
- 上传内容是明文 HTTP body（HTTPS 由服务地址决定），不要在文件里塞密钥。
- `artifact` 与本仓库的 Web 抓取能力无关：`WebSearch` / `WebFetch` 的后端是编译期固定的，不读这两个环境变量，也没有对应的用户配置项。
