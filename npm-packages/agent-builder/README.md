# Peri Agent Builder

本地单用户 Web 应用：配置 Peri Agent（指令、provider、MetaHarness、MCP），在同一个页面里与 Agent 对话，并让 Agent 在**一个显式授权的 workspace** 里通过真实 `peri-sandbox` 执行任务。API 与前端同源，只监听 `127.0.0.1`。

- 每个 Chat 在打开前先 acquire 一个 Sandbox holder；Sandbox 的 MCP URL 由服务端注入 `session.mcpServers`。
- Sandbox URL、gateway 凭据与 actor 凭据只存在于服务端；浏览器只看到消息、状态和稳定的错误码。
- workspace 身份是 canonical `peri+sandbox://<machineId>/<path>` URI；路径来自部署配置，不接受前端输入。
- Provider API key 逐条保存在共享 Provider SQLite registry（明文），只在 Providers 页面或 `POST/PUT /api/providers` 的 credential 请求体写入/替换/清除，永不读回；不读取或回落 Provider 环境变量。Store/Sandbox token 保持部署环境配置，不入应用库。所有凭据不返回 API、不写入日志。
- 新建页提供应用内置「Peri 官方默认模板」；主表单只有名称、Provider、模型，高级设置保留描述、指令、MCP 和能力。模板不保存、不建 Chat、不外呼；覆盖先确认、可一步撤销，手动编辑后撤销失效。
- 左侧和移动导航的 `Providers` 管理共享连接。没有 Provider 时可在 Agent 草稿内添加，成功后选中且保留其他输入。只有一个凭据已配置的 Provider 可自动选择，多条必须显选。

## 1. 安装（private tarball）

需要 Bun ≥ 1.4.3（服务端使用 `Bun.serve` 与 `bun:sqlite`）。不需要 Vite dev server，也不需要仓库路径。

把打包输出的**整个目录**搬到目标机器：其中包含四个 tarball、`install.sh` 和 `agent-builder-pack-manifest.json`。脚本从自身目录解析四包，不依赖打包机器路径；先检查路径，再在新目录安装：

```bash
sh /path/to/moved-kit/install.sh --dry-run
sh /path/to/moved-kit/install.sh ~/peri-agent-builder
```

`--dry-run` 只核对四包存在并显示解析路径，不安装、不修改目标。真实安装会拒绝已有 `package.json` 的目标；套件可在安装前整体移动。下面是手动安装的等价步骤。agent-builder 的 manifest 只声明版本（不允许 `file:`），因此用 `overrides` 把三个本地依赖固定到 tarball；不要省略这一步，否则 Bun 会去 registry 找同版本：

```bash
mkdir -p ~/peri-agent-builder && cd ~/peri-agent-builder
bun init -y
node - <<'NODE'
const fs = require('node:fs');
const p = JSON.parse(fs.readFileSync('package.json', 'utf8'));
p.overrides = {
  '@peri-code/sdk': 'file:/ABS/PATH/peri-code-sdk-0.0.0.tgz',
  '@peri-code/rivet': 'file:/ABS/PATH/peri-code-rivet-0.1.0.tgz',
  'peri-sandbox': 'file:/ABS/PATH/peri-sandbox-0.1.0.tgz',
};
fs.writeFileSync('package.json', `${JSON.stringify(p, null, 2)}\n`);
NODE
bun add /ABS/PATH/peri-code-agent-builder-0.0.0.tgz
```

若 Bun 阻止生命周期脚本，先信任本安装需要的两项，再重新安装：

```bash
bun pm trust peri-sandbox @openuidev/lang-core
bun install
```

内嵌引擎还需要平台包 `@rivetkit/engine-cli-<platform>`（rivetkit 的 optional dependency）。部分镜像会跳过它；缺失时 `doctor` 仍显示配置完整，但启动引擎会失败。可安装对应平台包，或在启动前设置 `PERI_RIVET_ENGINE_BINARY` 指向已有的 `rivet-engine`（wrapper 仍会应用小磁盘日志配额）。

```bash
cp node_modules/@peri-code/agent-builder/.env.example .env
# 编辑 .env：Store、workspace；Provider API key 启动后在 Providers 管理页或 POST /api/providers 填写（见 §5.2）
./node_modules/.bin/agent-builder doctor
./node_modules/.bin/agent-builder start
```

浏览器打开 `http://127.0.0.1:3000`。`doctor` 退出码：`0` 基础设施配置可启动（不代表 Provider 已配置或认证）；`2` 基础设施被阻塞（例如缺 Store token/WASM）；`1` 环境或目录非法。doctor 不读 Provider DB、不报告模型 key 的环境存在状态、不启动 engine、不外呼；SQLite 凭据在管理页逐项检查。

## 2. 配置

`cp .env.example .env` 后按需修改。Bun 启动时自动加载 `.env`；所有相对路径都相对**启动目录**解析。

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `HOST` | `127.0.0.1` | 唯一允许值 |
| `PORT` | `3000` | Web + API 端口 |
| `DATABASE_PATH` | `.data/agent-builder.db` | 应用配置库；目录 0700，库与 WAL/SHM 0600，父路径任一既有祖先不能含 symlink |
| `PERI_WORKSPACE_ID` | `agent-builder` | 只用于展示 |
| `PERI_WORKSPACE_PATH` | 启动目录 | **唯一授权 workspace**；必须是已存在的规范真实目录，不能是 symlink |
| `PERI_STORE_URL` | `http://127.0.0.1:8091/db/local_default/agent_builder/main` | Peri Store 数据库 URL |
| `PERI_STORE_TOKEN` | 未设置 | 空值视为未配置；只在服务端使用 |
| `PERI_ENGINE_PORT` | `8961` | 内嵌 rivet engine 端口 |
| `PERI_MACHINE_ID` | 数据目录内 `machine-id` | 可选 UUID；未设置时生成到 `<DATABASE_PATH 目录>/machine-id`（0600）并跨重启复用 |
| `RIVETKIT_STORAGE_PATH` | CLI 未设置时设为 `<DATABASE_PATH 目录>/rivet`；源码 `bun run start` 由 `scripts/api.sh` 解析（shell env > 包根 `.env` > 包根 `.data/rivet`） | 原生引擎在进程启动时读取；不能回退到 `~/.rivetkit` |
| `PERI_SANDBOX_ENABLED` | `false` | 打开后每个 Chat 使用真实 Sandbox |
| `PERI_SANDBOX_PERI_PATH` | `~/.peri/peri` | Peri 可执行文件；必须存在 |
| `PERI_SANDBOX_SETTINGS` | 未设置 | 可选已有 settings.json（0600）；留空则在 `<数据目录>/sandbox/settings.json` 生成 |
| `PERI_SANDBOX_ROOT` | `<数据目录>/sandbox/sandboxes` | Sandbox 实例状态目录（0700，不能与 workspace 互相包含） |
| `PERI_SANDBOX_PROFILE` | `default` | peri-sandbox profile 名 |
| `PERI_SANDBOX_TOKEN` | 自动生成 | actor/HTTP 凭据；留空时生成到 `<数据目录>/sandbox-token`（0600） |
| `PERI_SANDBOX_GATEWAY_TOKEN` | `default` | 本地 rivet gateway 凭据，**不是** actor 授权 |
| `PERI_SANDBOX_STARTUP_TIMEOUT_MS` / `PERI_SANDBOX_STOP_TIMEOUT_MS` | `90000` / `20000` | 环境启动/停止预算 |
| `PERI_ALLOWED_ORIGINS` | 同源 | 额外允许的浏览器 Origin（逗号分隔，仅支持回环主机） |
| `WEB_PORT` | 未设置 | 仅 Vite 开发时允许该 origin；生产不需要 |
| `PERI_WEB_ROOT` | 包内 `dist/` | CLI 自动设置；自定义静态目录时显式指定 |

源码 `bun run start` 时，`scripts/api.sh` 在 Bun 之前解析并导出 `RIVETKIT_STORAGE_PATH`：shell 中的非空值优先，否则取包根 `.env` 中的同名值，最后回退到包根 `.data/rivet`。安装后的 CLI 只有在该变量未设置时才使用上表默认值，显式环境变量或 `.env` 值优先。

Sandbox 额外要求：`PERI_WORKSPACE_PATH` 必须是 **0700** 真实目录。部署级原因会出现在 `/api/capabilities` 与 `doctor` 中，例如 `sandbox_workspace_not_private`、`sandbox_peri_missing`。

## 3. 实际使用：Chat + Sandbox

1. 打开 `Providers` 添加共享连接，在 password 输入框写入 SQLite API key；可替换/清除，永不读回，没有凭据来源选择。再新建 Agent，应用「Peri 官方默认模板」，填名称、Provider 和模型（模型建议可编辑，不保证供应商当前支持）。
2. 在 `工具` 页把 `McpMiddleware` 设为「开」（或「默认」）。Sandbox 使用 MCP；「默认」时该次 open 会自动启用 MCP，显式设置为「关」时创建对话会以 `sandbox_mcp_disabled` 拒绝，不会静默改回。
3. 保存后进入「对话」→「新建对话」选择该 Agent。创建时会先启动 Sandbox；成功后聊天页显示 `Sandbox 已就绪`。
4. 发送任务，例如：
   > 在工作区根目录创建 `hello.txt`，写入当前时间，然后用 `ls -l` 确认。
5. Agent 的回复流式显示；Sandbox 工具调用由本地引擎经注入的 MCP URL 执行。工具参数/结果不会把 token 发到浏览器。
6. 「关闭对话」：先关闭 Chat actor；确认后才释放该 Chat 的 Sandbox holder。多个 Chat 共用同一 workspace 时，只有最后一个确认释放才会停止环境。确认关闭后从对话列表再次进入仍会回读 Store 历史，并为该 `sessionId` 建立新运行，不复用旧执行或凭据快照。
7. 「删除」：仅在 Chat 与 Sandbox 都确认关闭后删除本地映射；Store 中的会话历史保留。关闭未确认时返回 409 `close_incomplete`，页面保留该行和 `Sandbox 关闭未确认` 状态，可重试关闭/删除。
8. 停止服务：在启动终端 `Ctrl+C`（或 `SIGTERM`）。顺序是停止接收请求 → 排空已准入操作并关闭所有 Chat → 仅释放确认关闭 Chat 的 Sandbox holders → `owner.closeAll` → 仅在 Chat 与 host 都确认后关闭 sandbox adapter → 关闭 client/registry。未确认步骤或超时均非零退出，不打印关闭成功；不跨过失败步骤全局释放资源，未完成的清理 Promise/责任保留到进程退出，状态目录不删除。

浏览器刷新或短暂 SSE 断连**不会**释放 Sandbox；holder 绑定 Chat 运行生命周期，不是 HTTP 连接生命周期。取消一次回复也不会删除 Chat/Sandbox。

## 4. 停机与状态目录

- 应用 SQLite：`<DATABASE_PATH 目录>`（0700）；`agent-builder.db*` 0600。
- 机器身份：`<DATABASE_PATH 目录>/machine-id`（0600）。不要随手删除或修改；Store 会话归属与 workspace URI 都依赖它。
- Sandbox actor 凭据：`<DATABASE_PATH 目录>/sandbox-token`（0600）。
- 生成的 Sandbox settings：`<DATABASE_PATH 目录>/sandbox/settings.json`（0600，含 peri 路径与 sha256，无 token）。
- Sandbox 实例状态：`PERI_SANDBOX_ROOT`（0700）。只有 owner 与 adapter 都确认关闭后才可清理；服务不会在 close incomplete 时删除它。
- 应用 SQLite 按 `server/db/migrate.ts` 的 `LATEST_SCHEMA_VERSION` 顺序迁移，当前为 **v3**：v1 建 `agents`、`chats`；v2 增 `providers` 与 `agents.provider_id/revision`；v3 增 `platform_*` 平台表并扩展 `agents` 列。引用以 FK 列为唯一权威，config JSON 的 provider 只存 `{model}`，读取时转换为 reference DTO。迁移原子、失败回滚；旧 inline/null 不批量改写、不按同名自动关联。
- SQLite、WAL/SHM 及手工备份均可能含明文 key，文件须 0600、父目录 0700；应用拒绝共享父目录、数据库父路径任一既有祖先的 symlink，以及 DB/WAL/SHM symlink，不 realpath 归一化、不擅自 chmod 共享目录；缺失目录逐级校验后创建。备份应由操作者自行以同等私有权限保存。本库不是 SDK `ChatDatabase`，不保存可重放的执行状态。
- 首次运行在 per-chat 队列中同步深拷贝 Agent/Provider/credential 并登记引用，再 acquire Sandbox。之后 open/send/cancel/stream 复用该 target，只合成本次 Sandbox 注入；Provider/Agent 修改仅影响后续新运行。close/rollback 未确认时保留 target 和引用，实际清理确认后释放。重启后只加载历史并建立新运行，不恢复旧执行或凭据快照。
- Provider 被 Agent 或 pending/open/关闭未确认运行引用时删除返回 409 `provider_in_use`。Agent 更换 Provider 不会解除旧运行引用，不级联、不 kill Chat。运行引用检查和事务提交间无 await，仅保证本地单进程。

## 5. API

所有接口与页面同源，只监听 `127.0.0.1`。请求体是 JSON 时必须带 `Content-Type: application/json`；对象是 strict 的，未知字段返回 400，不会静默忽略。下文 `<...>` 是占位符。Provider API key 仅可放 Providers POST/PUT 的 credential 请求体；不要放 URL、Agent/MCP 配置、终端参数或导出。Store/Sandbox token 仍只来自部署配置。

统一错误体：

```json
{ "error": { "code": "invalid_input", "message": "...", "details": {}, "retryable": false } }
```

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/health` | `{ok, engine:{mode,status,reasons?}, runtime}`；`ok` 只表示进程存活并能响应 |
| GET | `/api/capabilities` | 能力、provider 是否配置、`chatRuntimeReady`、`sandbox` 部署状态；不含任何密钥 |
| GET/POST | `/api/providers` | 安全列表 / 新建共享 Provider；GET 无 key 或 masked-secret 字段 |
| GET/PUT/DELETE | `/api/providers/:id` | PUT/DELETE 必须 If-Match revision；被引用时 `provider_in_use` |
| GET/POST | `/api/agents` | 列表 / 创建 `AgentDraft` |
| GET/PUT/DELETE | `/api/agents/:agentId` | PUT/DELETE 必须 If-Match revision；仍被 Chat 引用时 `agent_has_chats` |
| POST | `/api/agents/preview` | 只组合并脱敏 target，不落库、不 acquire Sandbox |
| GET/POST | `/api/chats` | 列表（含每个 Chat 的 `sandbox.state`；`sessionId` 可能为 `null`）/ 创建并启动 Sandbox |
| GET | `/api/chats/:chatId/events` | SSE；`sinceSeq`/`Last-Event-ID` 重放 |
| POST | `/api/chats/:chatId/send` | `{requestId,text}`；幂等 |
| POST | `/api/chats/:chatId/cancel` | 停止当前回复 |
| POST | `/api/chats/:chatId/close` | 200 `closed` 或 202 `close_incomplete` |
| DELETE | `/api/chats/:chatId` | 204；Chat 与 Sandbox 未确认关闭时 409 `close_incomplete` |

`/api/health` 的 `ok` 只表示 HTTP 进程存活并能响应，**不**表示聊天运行时就绪；就绪看 `engine.status`（`ready` / `starting` / `unknown` / `unavailable`）。`ok:true` 且 `engine.status` 非 `ready` 是正常组合，不是健康检查故障。

`GET /api/capabilities` 是发布面的自描述入口：`metaHarnessKeys` 列出 SDK 接受的键，`providers[].configured` 仅表示同协议至少一条共享 SQLite Provider 存有 key，不代表选中 Provider 可用，更不能让旧 inline 执行。`chatRuntime` 仍是配置模式（`wasm` / `not_configured`），不代表当前可执行；`chatRuntimeReasons` 列出原因，`chatRuntimeReady` 只在聊天运行时真正就绪（引擎 `ready`）时为 `true`，`historyPersisted` 跟随 `chatRuntimeReady`。UI 在未就绪时禁用创建/发送；`sandbox` 描述部署状态。特定 Provider 凭据状态按 `/api/providers` 的 id/hasCredential 判断，不代表已认证；页面和首次 Chat 都按确切引用检查。保存和预览不要求凭据或基础设施就绪。

### 5.1 创建 Agent（只保存配置）

`POST /api/agents` 与 `POST /api/agents/preview` 使用同一个 `AgentDraft` 请求体。创建只写本地 SQLite，preview 只读取选中 Provider 来组合安全 target；二者不启动 Chat、不连接 MCP、不 acquire Sandbox，不要求 key/Store 已配置。reference 的 Provider 必须存在，config/metaHarness/MCP 保持结构校验。

| 字段 | 必填 | 规则 |
| --- | --- | --- |
| `name` | 是 | trim 后 1–80 字符 |
| `description` | 否 | trim 后最多 400 字符，缺省 `""` |
| `config` | 是 | 对象；可写 `{}`，各子字段取缺省 |
| `config.instructions` | 否 | 最多 32768 字符，缺省 `""` |
| `config.provider` | 否 | 推荐 `{kind:"reference",providerId:"<PROVIDER_UUID>",model:"<MODEL_ID>"}`，strict 禁止夹带 type/baseUrl/key；模型 1–200 字符。旧内嵌 `{id,type,model,baseUrl?}` 和 null 继续读写/预览，但不能启动新运行；须显式选择共享 Provider，不自动关联同名 id |
| `config.metaHarness` | 否 | `string -> boolean`；键名必须来自 `GET /api/capabilities` 的 `metaHarnessKeys`，未知键返回 400；缺省 `{}` |
| `config.mcpServers` | 否 | 名称 1–64 字符，仅 `[A-Za-z0-9_-]`；`peri-workspace-sandbox` 是保留名；缺省 `{}` |
| `config.mcpServers.<name>.type` | 是 | `http` 或 `stdio`；`acp` 返回 400 `unsupported_mcp_type` |
| `config.mcpServers.<name>.url` | http 时是 | 仅 `http`/`https`；可选 `headers: string -> string` |
| `config.mcpServers.<name>.command` | stdio 时是 | 非空命令；可选 `args: string[]`、`env: string -> string` |

MCP 的 `headers`/`env` 会随 Agent 配置保存在本地 SQLite，并出现在单条 `GET /api/agents/:agentId` 响应里；不要在这里复用 provider key。

待办实现者与审查修改者这两个用途的完整请求：

<!-- api-contract:agent-todo -->
```json
{
  "name": "Todo Implementer",
  "description": "Implements and updates the todo application in the authorized workspace",
  "config": {
    "instructions": "Build a small todo app in the workspace root. Implement add, complete and filter, then run the available checks and report what changed.",
    "provider": {
      "kind": "reference",
      "providerId": "<ANTHROPIC_PROVIDER_UUID>",
      "model": "<ANTHROPIC_MODEL_ID>"
    },
    "metaHarness": {
      "McpMiddleware": true
    }
  }
}
```

<!-- api-contract:agent-reviewer -->
```json
{
  "name": "Code Reviewer",
  "description": "Reviews the todo application and proposes focused fixes",
  "config": {
    "instructions": "Review the todo app in the workspace. Report concrete defects and make only focused fixes; do not rewrite unrelated files.",
    "provider": {
      "kind": "reference",
      "providerId": "<OPENAI_PROVIDER_UUID>",
      "model": "<OPENAI_MODEL_ID>"
    },
    "metaHarness": {}
  }
}
```

把示例分别保存为文件后可直接调用：

```bash
BASE_URL=http://127.0.0.1:3000
curl -sS -X POST "$BASE_URL/api/agents" -H 'content-type: application/json' --data-binary @agent-todo.json
curl -sS -X POST "$BASE_URL/api/agents" -H 'content-type: application/json' --data-binary @agent-reviewer.json
curl -sS -X POST "$BASE_URL/api/agents/preview" -H 'content-type: application/json' --data-binary @agent-todo.json
```

创建成功返回 201 `{"agent":{"id","name","description","createdAt","updatedAt","revision","config"}}`。列表只返回摘要；单条 GET 返回完整 config/revision。PUT/DELETE 必须带 `If-Match: "<revision>"`（也接受未加引号的整数）；缺失或陈旧返回 409 `revision_conflict`，PUT 成功 revision+1，用响应更新基线。旧无 If-Match 客户端必须升级，这不是完全无感的 wire 兼容。冲突不能自动覆盖脏草稿，应确认后重载最新版本。

预览返回 200 `{target,warnings}`，凭据替换为 `[redacted]`，不落库、不 acquire Sandbox；缺 key 时仍可预览并警告。示例均使用共享 reference，先在 Providers 创建对应协议的条目并替换 UUID；旧 inline 仅保留编辑和读取，不再通过环境 key 执行：

```json
{"name":"Peri Agent","config":{"provider":{"kind":"reference","providerId":"<PROVIDER_UUID>","model":"<MODEL_ID>"},"instructions":"","metaHarness":{},"mcpServers":{}}}
```

### 5.2 Providers 与 CAS

1. POST 接受 `name`（1–80）、`type`（anthropic/openai）、`baseUrl`（null 或严格 URL），可选 `credential:{action:"replace",value:"<NEW_KEY>"}` 或 `{action:"clear"}`。key 只写该 Provider 的 SQLite `api_key`；可无 key 保存，但不能启动新运行。兼容字段 `credentialSource` 只接受 `local`，省略默认 `local`；旧请求指定 `env` 明确返回安全 400 `invalid_input`，不会读取环境 key。
2. PUT 同上但不接受 `type`（创建后不可改）；省略 credential 保留已存 key，replace 必须非空且不能是 `[redacted]`，clear 清除 key。元数据、凭据与 revision CAS 同事务。旧 v2 env 行只读投影为 `local/hasCredential:false`，列表和编辑显示待填写 SQLite key；只在显式保存时将该行 `credential_source` 改为 `local`，不导入环境、不自动批量转换。省略/clear 仍无 key，显式 replace 后才能运行。v2 迁移原文不变。
3. GET 显式投影 `id/name/type/baseUrl/credentialSource/hasCredential/revision/agentCount/runtimeCount`（来源仅 `local`），无 key、无 masked-secret 字段。hasCredential 仅表示此 Provider 存有 key，不表示认证通过；无连接测试、凭据读取/复制/导出接口。
4. Provider PUT/DELETE 与 Agent PUT/DELETE 都使用 If-Match。Provider 名称、key 和 URL 修改仅影响新运行；删除被引用返回 `provider_in_use`，版本冲突是独立 `revision_conflict`。
5. 自定义 URL 仅 http/https，无 userinfo/query/fragment，不接受首尾空格或控制字符；远端必须 https，http 仅 loopback。null 使用协议默认 endpoint。改变 origin 仍使用已有 SQLite key 时，UI 先确认，API 要求 `confirmCredentialOriginChange:true`；否则 409 `credential_origin_confirmation_required`。该确认不绕过 URL 校验；信任模型为受同源保护的本地单用户主动配置，不是远程多人代理。

<!-- api-contract:provider-create -->
```json
{"name":"My Anthropic","type":"anthropic","baseUrl":null}
```

该请求可在 `/api/providers` POST 创建未配置凭据的条目。不要在 shell 参数、日志或示例文件写真实 key；优先在 Providers 页面填写。脚本场景把请求体写入权限 0600 的临时文件，再用 `--data-binary @file` 发送（`<PROVIDER_API_KEY>` 是占位符）：

```bash
PROVIDER_BODY="$(mktemp)"
chmod 600 "$PROVIDER_BODY"
cat > "$PROVIDER_BODY" <<'JSON'
{"name":"My Anthropic","type":"anthropic","baseUrl":null,"credential":{"action":"replace","value":"<PROVIDER_API_KEY>"}}
JSON
curl -sS -X POST "$BASE_URL/api/providers" \
  -H 'content-type: application/json' \
  --data-binary @"$PROVIDER_BODY"
rm -f "$PROVIDER_BODY"
```

`<PROVIDER_API_KEY>` 只是占位符。`--data-binary @file` 只把文件名放进 curl 的进程参数，curl 阶段 key 不进入命令行；临时文件仍是明文，必须限制权限并在请求后立即删除。上面的 heredoc 也只写占位符：真实 key 要用编辑器或密码管理器写入该文件，或先 `read -rs` 读入变量后生成 JSON。不要用 `--data '{"...":"$KEY"}'`，也不要把真实 key 直接敲进 heredoc 或命令行，否则会进入 shell 历史和进程参数。PUT 元数据请求示例（必须带 `If-Match: "<revision>"`，省略 credential 保留 key）：

<!-- api-contract:provider-update -->
```json
{"name":"Renamed","baseUrl":null}
```

PUT 与删除的版本头示例：

```bash
curl -sS -X PUT "$BASE_URL/api/providers/<PROVIDER_UUID>" -H 'content-type: application/json' -H 'If-Match: "1"' --data '{"name":"Renamed","baseUrl":null,"credentialSource":"local"}'
curl -sS -X DELETE "$BASE_URL/api/providers/<PROVIDER_UUID>" -H 'If-Match: "2"'
curl -sS -X PUT "$BASE_URL/api/agents/<AGENT_ID>" -H 'content-type: application/json' -H 'If-Match: "1"' --data-binary @agent-todo.json
```

### 5.3 创建 Chat（启动真实执行）

`POST /api/chats` 的 body：

<!-- api-contract:chat-create -->
```json
{
  "agentId": "<AGENT_ID>"
}
```

| 字段 | 必填 | 规则 |
| --- | --- | --- |
| `agentId` | 是 | 已创建 Agent 的 UUID；从 `POST /api/agents` 或 `GET /api/agents` 取得 |
| `traceId` | 否 | 1–200 字符；只接收，不持久化、不回显 |

创建成功返回 201 `{"chat":{"chatId","agentId","agentName","sessionId","createdAt","updatedAt","sandbox":{"state","reason"}}}`。后续 SSE、send、cancel、close、delete 都用这个 `chatId`。

`sessionId` 在创建响应里可以为 `null`，表示该 chat 尚未绑定 Store 会话。它在该 chat **首次订阅 `GET /api/chats/:chatId/events`** 或**首次 `POST /api/chats/:chatId/send`** 后写入，之后 `GET /api/chats` 返回非空值；`close` 不负责创建会话。这里的 `sessionId` 是 Store 会话 id；`/v1` 的 `session.id` 是 platform session id，并等于该会话对应的 `chatId`，两个接口面的边界见 §5.4。

创建 Chat 不是“保存配置”，而是一次真实执行准入：

1. **基础设施必须就绪。** `chatRuntime: wasm` 要求 Store token、可连通的 Store、SDK WASM 与 `RIVETKIT_STORAGE_PATH`；就绪时 `historyPersisted` 为 true。缺项时 `POST /api/chats`、`GET /api/chats/:chatId/events`、`POST /api/chats/:chatId/send`、`POST /api/chats/:chatId/cancel` 返回 503 `engine_unavailable`，`details.reasons` 是稳定原因码；`close` 仍返回 200/202，`DELETE` 在关闭未确认时返回 409 `close_incomplete`。基础设施探针不读取 Provider key，既有 Store 认证、WASM、Rivet 与 Sandbox 门禁仍保留。
2. **首次目标必须明确关联共享 SQLite Provider 且存有 key。** 只按 reference 的 Provider UUID 解析该行 `api_key`，不按协议或同名 ID 认领，不回落到环境。旧 inline/null 仍可读写、编辑模型与预览，但新运行返回 409 `not_ready`（`provider_missing`）；请选择共享 Provider。共享行无 key 返回 `api_key_missing`，请在 Providers 管理页或 `PUT /api/providers` 的 credential 中填写；这些拒绝不发模型请求。hasCredential 不表示认证成功。已冻结的 Chat 不因随后 replace/clear key 而阻断 send/cancel/close；外部撤销 key 仍可能使实际调用失败。
3. **启用 Sandbox 时还要满足部署前提。** `PERI_SANDBOX_ENABLED=true` 时，创建会先 acquire 真实 Sandbox 再打开 Chat；workspace 必须是 0700 规范真实目录，`PERI_SANDBOX_PERI_PATH` 必须指向可执行的 Peri，settings/token 等前提见 `doctor` 与 `/api/capabilities`。Agent 显式设置 `McpMiddleware: false` 会以 409 `not_ready`（`details.reason: sandbox_mcp_disabled`）拒绝，不会静默改回开启；Sandbox 失败也不会降级成无 Sandbox 的 Chat。
4. **没有“离线 Chat”模式。** 缺 Store 或 key 时只保留 Agent 配置与预览能力；`POST /api/chats` 按上面的错误返回。把 Sandbox 关闭只移除第 3 项前提，仍需第 1、2 项。

前提到位后的顺序沿用第 3 节与上表，不新增接口：创建 Agent → 预览（可选）→ `POST /api/chats` 获得 `chatId` → `GET /api/chats/:chatId/events?sinceSeq=0` 订阅 SSE → `POST /api/chats/:chatId/send` 发送 `{requestId,text}` → 需要时 `POST /api/chats/:chatId/cancel` → `POST /api/chats/:chatId/close` → 确认关闭后 `DELETE /api/chats/:chatId`。`requestId` 由客户端生成；同一 `requestId` 配不同 `text` 返回 409 `request_conflict`。`text` 上限见 `GET /api/capabilities` 的 `sendTextMaxLength`。`close` 返回 202 `close_incomplete` 时先重试关闭，不要直接删除；已确认关闭的 Chat 可再次 `GET .../events` 或 `send`，服务按 Store 历史建立新运行并返回现有消息，`close_incomplete` 期间新进入仍返回 409 `closed`。

**创建失败后的 Chat 行。** 创建先写入 Chat 行，再打开 actor；打开失败会回滚该行。若回滚无法确认 actor 关闭（`actor.close()` 失败）或 Sandbox holder 未确认释放，该行会保留以便诊断，此时 `sessionId` 仍为 `null`。引擎在创建过程中失联时，`POST /api/chats` 返回 503 `engine_unavailable`（引擎在启动阶段就失败的话，创建发生在写行之前，不会留下行）；若引擎仍报告 `ready`，返回的是打开失败本身的错误，例如 500 `internal_error`。保留的行不会让后续接口退化为 500：`GET /api/chats` 仍为 200；运行时不可用期间，`GET /api/chats/:chatId/events` 与 `POST /api/chats/:chatId/send` 返回 503 `engine_unavailable`；`DELETE /api/chats/:chatId` 返回 409 `close_incomplete`（`details.actorClosed: false`；若只是 Sandbox 未确认释放，则为 `true`，原因码在 `details.sandboxErrors`）。运行时恢复（重启 API 并重新启动引擎）后可再次 `DELETE` 完成清理；服务不会为了“能删掉”而释放未确认关闭的运行。

SSE 事件：`snapshot` / `message` / `state` / `reset` / `closed`。服务端用 SDK 的 `ChatState` 做唯一投影；SSE 只负责传输、重连与序号排序。回读期间收到更高 binding revision 时，按新信号串行回读 actor 的 `facts()` 确认，不以 live fact 授予权威；同代次或尚未确认的信号不会驱动自动重试循环。回读失败会发送 `reset` 并关闭旧订阅；新连接可 singleflight 重试，仍失败则在提交 SSE 响应前返回可重试的安全 503 `engine_unavailable`，不返回成功空流。

### 5.4 `/v1` 资源接口

应用进程同时在 `/v1` 暴露面向外部客户端的资源接口，与 `/api` 使用不同的请求/响应信封。每个请求需要 `Authorization: Bearer <任意非空值>`（只判存在性），分页使用 `object/list/data/first_id/last_id/has_more`，错误使用 `{error:{type,code,message,param}}`。

启动路径随目录性质不同：源码工作目录用 `bun run start`（跑 `server/main.ts`），改代码后要 `bun run build:server` 才会更新 `dist/server/main.js`；安装后的 kit 用 `./node_modules/.bin/agent-builder start`（跑 `dist`）。`/v1` 只存在于跑本仓库源码、或已重建 `dist` 的进程里。

执行 `/v1` Session 前，对未指定 Provider 的 Agent（`/v1` 创建的 Agent 默认如此），部署里必须恰好有一条 Provider 记录（且存有凭据），且 Store 可达；Provider 通过同源管理接口 `/api/providers` 配置（不需要 `Authorization`，只接受回环 Host/同源 Origin；Web 页面使用同一接口），`/v1` 不提供 Provider 管理。无浏览器的最小 curl 步骤见 [`docs/agents-api.md`](docs/agents-api.md) 的 §2。`doctor` 只校验配置存在，不代表 Store 或引擎可达。`PERI_STORE_URL` 的形状取决于所连服务（路径形式并非所有实现支持），`PERI_ENGINE_PORT` 与其 +1 端口都要空闲，macOS 上 `/tmp` 是符号链接、绝对路径要先 realpath。

两个接口面共享同一本地存储，但列表可见性只有单向：`POST /v1/agents/sessions` 创建时会同时建立 platform session 与对应 chat（chat 的 id 就是 `/v1` 的 session id），所以 `/v1` 创建的会话**会**出现在 `GET /api/chats` 列表里（该列表没有来源过滤）；`/v1` 的会话列表只读 platform session，**看不到**通过 `POST /api/chats` 创建的对话。`/api/chats` 创建的 chat 不是 platform session，用它的 `chatId` 请求 `GET /v1/agents/sessions/:id` 返回 404。`docs/agents-api.md` §4.2 从 `/v1` 侧给出同一契约。

端点、字段、幂等（`Idempotency-Key`）、SSE 与 `Last-Event-ID` 续传、本地降级行为见 [`docs/agents-api.md`](docs/agents-api.md)；当前契约是 57 个操作 / 34 条路径。

## 6. 安全边界

- 仅绑定 `127.0.0.1`；`Host` 必须是回环主机名；带 `Origin` 的请求必须命中显式 allowlist；`Sec-Fetch-Site: cross-site` 直接 403。防止任意本地/远程网页操作高权限工具。
- workspace 只来自 `PERI_WORKSPACE_PATH`；没有前端路径输入接口。
- Sandbox 注入的 MCP 名称 `peri-workspace-sandbox` 保留；用户同名配置在保存时 400，历史行在打开时明确失败，不覆盖、不转发。
- 预览把 provider key、Store token、用户 MCP header/env 全部替换为 `[redacted]`；Sandbox 的 URL 与两份凭据从不进入预览。
- 后端公共错误只返回固定文案与白名单码/诊断字段；日志不序列化任意 Error 的 message、name、stack、cause。原始异常作为进程内 cause 或待关闭责任保留。
- 同一 Chat 的 create/open/send/cancel/stream 与 close/delete 按进程内队列排序；关闭开始后不再接受新工作，未确认的 close 持续阻塞新进入。关闭确认后释放本次运行与 Sandbox holder；再次进入（`GET /events` 或 `send`）会按 Store 中持久化的 `sessionId` 建立新运行并回读历史，不复活旧 actor、旧 target 或凭据快照。
- 切换 Chat 或手动重连会清空旧流投影；迟到的请求/事件不改变当前 Chat。binding 仅由 actor 的 `facts()` 快照确认，live fact 交由 SDK `ChatState` 过滤。

## 7. 开发

```bash
bun install
bun run dev          # API (WEB_PORT 开发 origin) + Vite :5173，/api 代理到 :3000
bun run start        # 仅 API；scripts/api.sh 解析私有 RIVETKIT_STORAGE_PATH（shell env > 仓库根 .env > .data/rivet）
bun run build        # vite build + bun build server/main.ts -> dist/server/main.js
bun test
bun run typecheck
bun run format:check
bun run pack:local -- --out /path/to/out --pack-dir /path/to/verified-pack
```

`pack:local` 要求显式 `--pack-dir`（或逐个指定三个 tarball），只读取已有且已验的 SDK/rivet/sandbox tarball，不重建它们的 `dist/wasm`；它构建本应用的 SPA 与 bundled server，校验 tarball 白名单/manifest，把三个依赖复制到输出目录，形成四包套件。`agent-builder-pack-manifest.json` 记录相对路径、每包 sha256/size 与明确 provenance；commit 仅标识源码基线，dirty 与 WASM 兼容未验证状态单独记录。最终真实 fresh install 是独立后续 gate；轻量 fixture/dry-run 不能代替它。

## 8. 已知限制

- 真实 LLM + 可写 Store 的端到端已在本机跑通（本地 `sqld` Store、真实模型对话，单主机单进程）；远程/托管 Store、凭据轮换与多实例接管仍未验证。路由/组件测试不能代替真实对话。
- SDK 的 prebuilt WASM 与当前 Rust 源码兼容性未重新构建验证；打包记录为 unverified。
- 内嵌引擎在磁盘极低时可能启动失败；`doctor` 会报告可用磁盘。
- **引擎启动失败。** 配置为 `wasm` 但引擎无法启动（常见原因：`PERI_ENGINE_PORT` 或其 +1 端口被占用、`RIVETKIT_STORAGE_PATH` 不存在）时：`/api/health` 启动中为 `unknown`/`starting`，失败后为 `"unavailable"`；`/api/capabilities` 的 `chatRuntime` 仍为 `wasm`，但 `chatRuntimeReady` 与 `historyPersisted` 均为 `false`，Chat 的创建、events、send、cancel 路由返回 503 `engine_unavailable`（`close` 仍为 200/202，`DELETE` 在关闭未确认时为 409 `close_incomplete`）。端口被占用等使 rivetkit 无法完成引擎启动时，rivetkit 会终止 API 进程（exit 1）。本应用的启动失败诊断只输出白名单字段（`code: "engine_unavailable"`、`status`、`reasons`、`enginePort`、`hint`；引擎自身启动失败时另带受限的 `group`），不包含原始异常文本或路径。修正后重启 API。
- 单进程、单用户设计：没有多租户、远程注册或多实例接管。

边界：`src` 不引入 SDK/rivet 运行时；`server` 不引入 `src`；单文件 <1000 行。
