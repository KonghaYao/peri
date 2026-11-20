# Peri Agent Builder

本地单用户 API 服务：配置 Peri Agent（指令、provider、MetaHarness、MCP），通过 `/v1` 与 Agent 对话，并让 Agent 在**一个显式授权的 workspace** 里通过真实 `peri-sandbox` 执行任务。服务只监听 `127.0.0.1`，只暴露 `/v1` 资源 API，没有静态页面或其他管理接口。

- 每个 Chat 在打开前先 acquire 一个 Sandbox holder；Sandbox 的 MCP URL 由服务端注入 `session.mcpServers`。
- Sandbox URL、gateway 凭据与 actor 凭据只存在于服务端；客户端只看到消息、状态和稳定的错误码。
- workspace 身份是 canonical `peri+sandbox://<machineId>/<path>` URI；路径来自部署配置，不接受请求输入。
- 单一 Provider 由 `PERI_PROVIDER_TYPE`/`PERI_PROVIDER_BASE_URL`/`PERI_PROVIDER_API_KEY`/`PERI_PROVIDER_NAME` 在启动时同步到私有 SQLite；未设置类型则不同步，存在多条记录时启动失败而不删除。Store/Sandbox token 保持部署环境配置，不入应用库。所有凭据不返回 API、不写入日志。

## 1. 安装（private tarball）

需要 Bun ≥ 1.4.3（服务端使用 `Bun.serve` 与 `bun:sqlite`）。不需要仓库路径或额外的构建工具。

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

启动前提：Peri Store 可连通（`PERI_STORE_URL`，需要有效的 `PERI_STORE_TOKEN`）、SDK WASM 资源存在、`RIVETKIT_STORAGE_PATH` 可写；启用 Sandbox 时还需要 `peri` 可执行文件、私有 settings 与 0700 workspace。`doctor` 覆盖这些配置检查，但不代表 Store 或引擎已实际就绪。

```bash
cp node_modules/@peri-code/agent-builder/.env.example .env
# 编辑 .env：Store、workspace、PERI_PROVIDER_*（Provider 只从环境变量同步，见 §2）
./node_modules/.bin/agent-builder doctor
./node_modules/.bin/agent-builder start
```

服务只提供 `/v1` API（默认 `http://127.0.0.1:3000/v1`），没有静态页面。`doctor` 退出码：`0` 基础设施配置可启动（不代表 Provider 已配置或认证）；`2` 基础设施被阻塞（例如缺 Store token/WASM）；`1` 环境或目录非法。doctor 不读 Provider DB、不报告模型 key 的环境存在状态、不启动 engine、不外呼。

## 2. 配置

`cp .env.example .env` 后按需修改。Bun 启动时自动加载 `.env`；所有相对路径都相对**启动目录**解析。

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `HOST` | `127.0.0.1` | 唯一允许值 |
| `PORT` | `3000` | API 端口 |
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
| `PERI_ALLOWED_ORIGINS` | 同源 | 额外允许的 Origin（逗号分隔，仅支持回环主机） |
| `PERI_PROVIDER_TYPE` | 未设置 | `anthropic` 或 `openai`；未设置时启动不同步 Provider |
| `PERI_PROVIDER_BASE_URL` | 未设置 | 可选 Provider URL；必须包含协议与主机 |
| `PERI_PROVIDER_API_KEY` | 未设置 | 写入本地 Provider 的凭据；永不回显或记录 |
| `PERI_PROVIDER_NAME` | `env` | 同步到本地 Provider 的名称 |

源码 `bun run start` 时，`scripts/api.sh` 在 Bun 之前解析并导出 `RIVETKIT_STORAGE_PATH`：shell 中的非空值优先，否则取包根 `.env` 中的同名值，最后回退到包根 `.data/rivet`。安装后的 CLI 只有在该变量未设置时才使用上表默认值，显式环境变量或 `.env` 值优先。

Sandbox 额外要求：`PERI_WORKSPACE_PATH` 必须是 **0700** 真实目录。部署级原因会出现在 `doctor` 中，例如 `sandbox_workspace_not_private`、`sandbox_peri_missing`。

**端口。** `PORT`（默认 `3000`）是 `/v1` 的监听端口；内嵌引擎另用 `PERI_ENGINE_PORT`（默认 `8961`）与其 +1 端口（默认 `8962`），这些端口都必须空闲。`HOST` 固定为 `127.0.0.1`，不要把端口转发或暴露到其他机器。端口被占用时引擎启动失败并报告 `engine_unavailable`，可用 `lsof -nP -iTCP:<port> -sTCP:LISTEN` 检查。

### Provider 同步规则

启动时若设置了 `PERI_PROVIDER_TYPE`：

- 零条 Provider：写入一条 `credential_source=local` 的记录，`revision=1`。
- 一条 Provider：仅当字段不同才更新 name/type/base_url/api_key，保留 id。
- 多条 Provider：启动失败，稳定错误码 `provider_env_conflict`，提示会点名 `PERI_PROVIDER_*`；不会删除任何记录。

未设置 `PERI_PROVIDER_TYPE` 时不改动数据库；重复启动同一环境是幂等的。日志只输出类型与动作（`provider: env <type> (<action>)`），不含 key。

## 3. 实际使用：Chat + Sandbox

1. 在 `.env` 设置 `PERI_PROVIDER_TYPE` 与 `PERI_PROVIDER_API_KEY`（可选 `PERI_PROVIDER_BASE_URL`/`PERI_PROVIDER_NAME`），启动时同步为唯一本地 Provider。
2. 通过 `POST /v1/agents` 创建 Agent；未显式指定 Provider 的 Agent 会使用该本地记录。Sandbox 使用 MCP，内置 MCP 能力按需启用。
3. 通过 `POST /v1/agents/sessions` 创建会话与 Chat。创建时若启用 Sandbox，会先启动 Sandbox；成功后会话事件可观察 `session` 状态。
4. 通过 `POST /v1/agents/sessions/:id/turns` 发送任务，例如：
   > 在工作区根目录创建 `hello.txt`，写入当前时间，然后用 `ls -l` 确认。
5. Agent 的回复通过 SSE 流式返回；Sandbox 工具调用由本地引擎经注入的 MCP URL 执行，工具参数/结果不会把 token 发给客户端。
6. 关闭会话并确认后才会释放该 Chat 的 Sandbox holder；多个会话共用同一 workspace 时，只有最后一个确认释放才会停止环境。确认关闭后再次读取历史会建立新运行，不复用旧执行或凭据快照。
7. 删除会话仅在 Chat 与 Sandbox 都确认关闭后删除本地映射；Store 中的会话历史保留。关闭未确认时返回 409 `close_incomplete`，可重试。
8. 停止服务：在启动终端 `Ctrl+C`（或 `SIGTERM`）。顺序是停止接收请求 → 排空已准入操作并关闭所有 Chat → 仅释放确认关闭 Chat 的 Sandbox holders → `owner.closeAll` → 仅在 Chat 与 host 都确认后关闭 sandbox adapter → 关闭 client/registry。未确认步骤或超时均非零退出，不打印关闭成功；不跨过失败步骤全局释放资源，未完成的清理 Promise/责任保留到进程退出，状态目录不删除。

客户端短暂 SSE 断连**不会**释放 Sandbox；holder 绑定 Chat 运行生命周期，不是 HTTP 连接生命周期。取消一次回复也不会删除 Chat/Sandbox。

### 最小客户端示例

```bash
API='http://127.0.0.1:3000/v1'
AUTH='Authorization: Bearer local-dev-key'   # 任意非空值；服务只检查头存在

# 1) 建 Agent；model 必须是 PERI_PROVIDER_* 所配 Provider 提供的模型
curl -sS "$API/agents" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"name":"my-first-agent","model":"<MODEL>","instructions":"Reply briefly."}'

# 2) 建 Session；用上一步返回的 id 替换 <AGENT_ID>，记下响应里的 id 作为 <SESSION_ID>
curl -sS "$API/agents/sessions" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"agent_id":"<AGENT_ID>","environment":{"type":"none"},"input":"Hello!"}'

# 3) 读取 SSE 事件直到出现 agent.session.idle；流不会自行关闭，Ctrl+C 结束
curl -N "$API/agents/sessions/<SESSION_ID>/events" -H "$AUTH" -H 'Accept: text/event-stream'
```

完整 TypeScript 客户端（含输入提交、turn 与 item 读取）见 [`docs/agents-api.md`](docs/agents-api.md) §2。

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

服务只暴露 `/v1`。每个请求需要 `Authorization: Bearer <任意非空值>`（只判存在性）；分页使用 `object/list/data/first_id/last_id/has_more`，错误使用 `{error:{type,code,message,param}}`。未匹配的 `/v1` 路径返回 404 `not_found`；没有其他管理端点。

Provider 只能通过 `PERI_PROVIDER_*` 环境变量管理，`/v1` 不提供 Provider 端点。

启动路径随目录性质不同：源码工作目录用 `bun run start`（跑 `server/main.ts`），改代码后要 `bun run build` 才会更新 `dist/server/main.js`；安装后的 kit 用 `./node_modules/.bin/agent-builder start`（跑 `dist`）。

端点、字段、幂等（`Idempotency-Key`）、SSE 重放与显式失败（501/503）见 [`docs/agents-api.md`](docs/agents-api.md)；当前契约是 57 个操作 / 34 条路径。

## 6. 安全边界

- 仅绑定 `127.0.0.1`；`Host` 必须是回环主机名；带 `Origin` 的请求必须命中显式 allowlist；`Sec-Fetch-Site: cross-site` 直接 403。防止未授权的跨站请求操作高权限工具。
- workspace 只来自 `PERI_WORKSPACE_PATH`；没有请求路径输入接口。
- Sandbox 注入的 MCP 名称 `peri-workspace-sandbox` 保留；用户同名配置在保存时 400，历史行在打开时明确失败，不覆盖、不转发。
- 组合预览 target 时把 provider key、Store token、用户 MCP header/env 全部替换为 `[redacted]`；Sandbox 的 URL 与两份凭据从不进入预览。
- 后端公共错误只返回固定文案与白名单码/诊断字段；日志不序列化任意 Error 的 message、name、stack、cause。原始异常作为进程内 cause 或待关闭责任保留。
- 同一 Chat 的 create/open/send/cancel/stream 与 close/delete 按进程内队列排序；关闭开始后不再接受新工作，未确认的 close 持续阻塞新进入。关闭确认后释放本次运行与 Sandbox holder；再次进入会按 Store 中持久化的 `sessionId` 建立新运行并回读历史，不复活旧 actor、旧 target 或凭据快照。
- binding 仅由 actor 的 `facts()` 快照确认，live fact 交由 SDK `ChatState` 过滤。

## 7. 开发

```bash
bun install
bun run dev          # 仅 API：scripts/api.sh --hot server/main.ts
bun run start        # 仅 API；scripts/api.sh 解析私有 RIVETKIT_STORAGE_PATH（shell env > 仓库根 .env > .data/rivet）
bun run build        # bun build server/main.ts -> dist/server/main.js（唯一构建产物）
bun test
bun run typecheck
bun run format:check
bun run pack:local -- --out /path/to/out --pack-dir /path/to/verified-pack
```

`pack:local` 要求显式 `--pack-dir`（或逐个指定三个 tarball），只读取已有且已验的 SDK/rivet/sandbox tarball，不重建它们的 `dist/wasm`；它构建本应用的 server bundle，校验 tarball 白名单/manifest，把三个依赖复制到输出目录，形成四包套件。`agent-builder-pack-manifest.json` 记录相对路径、每包 sha256/size 与明确 provenance；commit 仅标识源码基线，dirty 与 WASM 兼容未验证状态单独记录。最终真实 fresh install 是独立后续 gate；轻量 fixture/dry-run 不能代替它。

## 8. 已知限制

- 真实 LLM + 可写 Store 的端到端已在本机跑通（本地 `sqld` Store、真实模型对话，单主机单进程）；远程/托管 Store、凭据轮换与多实例接管仍未验证。测试不能代替真实对话。
- SDK 的 prebuilt WASM 与当前 Rust 源码兼容性未重新构建验证；打包记录为 unverified。
- 内嵌引擎在磁盘极低时可能启动失败；`doctor` 会报告可用磁盘。
- **引擎启动失败。** 配置为 `wasm` 但引擎无法启动（常见原因：`PERI_ENGINE_PORT` 或其 +1 端口被占用、`RIVETKIT_STORAGE_PATH` 不存在）时：启动日志与引擎诊断报告 `engine_unavailable`、`status` 与 `reasons`；`/v1` 会话创建与 turn 请求返回 503 `engine_unavailable`。端口被占用等使 rivetkit 无法完成引擎启动时，rivetkit 会终止 API 进程（exit 1）。本应用的启动失败诊断只输出白名单字段（`code: "engine_unavailable"`、`status`、`reasons`、`enginePort`、`hint`；引擎自身启动失败时另带受限的 `group`），不包含原始异常文本或路径。修正后重启 API。
- 单进程、单用户设计：没有多租户、远程注册或多实例接管。

边界：`server` 只有 API 进程，不提供静态资源；单文件 <1000 行。
