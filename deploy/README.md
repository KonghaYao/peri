# Peri Swarm（单机 Compose）

一个 compose 拉起 Peri Swarm core：

- `postgres`：`peri_session`（Peri 会话库）+ `peri_swarm_state`（Swarm 状态库）
- `rivet-engine`：独立运行的 Rivet 3.x 控制面，filesystem 存储
- `rivet-init`：一次性创建 `peri-swarm` node pool
- `peri-swarm`：网关二进制（HTTP API + 进程内 ACP Host），默认 2 个副本
- `nginx`：副本组前面的入口，负责 HTTP 转发与 SSE 透传

> 本目录尚未启动验证；下面是首次启动前的准备和检查项。

## 前置

- Docker + Docker Compose v2
- 磁盘至少 4 GiB 空闲，且保留 10% 以上（Rivet engine 的硬性要求）
- 一个模型 provider 的 API key

## 准备

1. 生成 `.env`：

```bash
cd deploy
cp .env.example .env
openssl rand -hex 24   # POSTGRES_PASSWORD
openssl rand -hex 32   # PERI_SWARM_API_KEY
openssl rand -hex 32   # RIVET_ADMIN_TOKEN
```

把三个值填进 `.env`；`POSTGRES_PASSWORD` 只用 URL 安全字符，因为它会拼进两个 DSN。

2. 填写模型配置：

`config/peri-swarm/settings.json` 已按 `settings.example.json` 预置了占位内容
（`REPLACE-WITH-PROVIDER-KEY` / `REPLACE-WITH-MODEL-ID`），它是 Compose secret，
已被 `.gitignore` 忽略。直接编辑它：

```bash
chmod 600 config/peri-swarm/settings.json
$EDITOR config/peri-swarm/settings.json
```

如果文件被删了，从示例复制：

```bash
cp config/peri-swarm/settings.example.json config/peri-swarm/settings.json
```

3. 启动：

```bash
docker compose up -d --build
```

（本次没有执行。）

## 端口

| 用途 | 端口 | 说明 |
| --- | --- | --- |
| 对外入口 | `${PERI_SWARM_PORT:-8080}` + `${PERI_SWARM_PUBLISH_HOST:-127.0.0.1}` | nginx 发布，正常客户端只用这个 |
| 副本直连 | `${PERI_SWARM_REPLICA_PORT_RANGE:-8100-8199}` | Docker 给每个副本分配一个空闲宿主端口，仅调试用 |

副本不发布固定端口，否则 `--scale` 会互相抢端口；范围由 Docker 逐个分配。
`docker compose ps` 的 PORTS 列就是各副本实际拿到的端口。正常流量走 nginx 的
容器网络，不经过这些宿主端口。

## 检查

```bash
docker compose config               # 先看变量展开是否正确（不启动容器）
docker compose ps                   # 2 个 peri-swarm 副本；rivet-init 应为 Exited (0)
curl -fsS http://127.0.0.1:8080/healthz
docker compose exec postgres psql -U peri -d peri_swarm_state -c '\dt'
docker compose exec --index=1 peri-swarm curl -fsS http://rivet-engine:6420/health
docker compose exec --index=1 peri-swarm sh -c \
  'curl -fsS -H "Authorization: Bearer $PERI_SWARM_RIVET_TOKEN" \
   "http://rivet-engine:6420/pools/peri-swarm/nodes?namespace=default"'
```

`\dt` 应看到 6 张 `swarm_*` 表（含 `swarm_meta`）；节点列表应看到 **2 个**
`connected: true` 的 node。两个副本应读到同一个 machine-id：

```bash
docker compose exec --index=1 peri-swarm cat /var/lib/peri-swarm/machine-id
docker compose exec --index=2 peri-swarm cat /var/lib/peri-swarm/machine-id
```

## 会话

客户端用 OpenAI 兼容 SDK 指向 `http://127.0.0.1:8080/v1`，Bearer 为
`PERI_SWARM_API_KEY`（示例见 `../peri-swarm/README.md` 的 Node SDK 一节）。

`environment.type=none` 可直接用；`openai_hosted` 本期没有 sandbox，会在创建时
显式失败（未配置 Bun 桥的既有语义）。

## 副本行为

- 两个副本共享同一个 engine、namespace/pool、两个数据库，以及同一个
  `peri-swarm-data` 卷（machine-id 和工作区同路径共享）。同一个
  `agent/<logicalId>` 同一时刻只在一个副本上运行。
- 入口任意请求都可以落到任意副本；action 由 engine 定位到承载 actor 的副本，
  不依赖粘滞会话。nginx 用 Docker DNS（10s 重解析）做轮询，扩缩容后不用重启。
- 扩副本：`docker compose up -d --scale peri-swarm=3`（会覆盖 `deploy.replicas`），
  端口范围要留够空闲端口。
- 在途 turn 的跨实例接管是设计非目标：杀掉承载 actor 的副本，该 turn 会 502，
  行保持 `running`；下一次输入触发只读恢复 + `session/load`。网络分区下的双写
  没有 fencing，不能按高可用验收。

## 注意

- 两个 DSN 必须指向不同数据库；不要把 `PERI_SWARM_STATE_STORE` 指向
  `peri_session`，否则启动会报「这是 Peri 会话库」。
- 状态库首次启动必须是空的；网关会自己建表并写 `swarm_meta`。
- `docker compose down -v` 会同时清空两个数据库、Rivet 的
  `rivet-log`/`rivet-objects` 和 `peri-swarm-data`。`rivet-log` 里有尚未落到
  对象存储的写入，**有数据时不要单独删它**。
- `peri-swarm` 在容器内绑 `0.0.0.0:8080`（端口映射需要），对外只发布
  `127.0.0.1`；启动日志里的 non-loopback WARN 是预期。
- 模型 key 只在 `config/peri-swarm/settings.json`（Compose secret）里；不要把它
  写进 `.env` 或提交。`settings.json` 仍是占位值时网关能启动，但第一次真实会话
  会因 key/model 无效失败。
- nginx 只做 HTTP 转发和 SSE 透传；TLS 与统一鉴权应由更外层入口负责。
- `rivetdev/engine` 的 tag 需要 pin；官方文档默认 `latest`，与仓库
  `rivetkit 3.1.1` 的兼容性需要实测。engine 的 `/health` 之后几秒内 API 可能
  返回 `depot.overloaded`，`rivet-init` 会重试。
- 若本机 buildkit 拉不到 Dockerfile frontend，或构建容器解析不了
  `deb.debian.org` / `static.crates.io`（OrbStack 默认网络会这样），用 host
  网络预构建，再跳过构建启动：

  ```bash
  docker buildx build --network=host --pull=false \
    -f images/Dockerfile --target peri-swarm -t peri-swarm:local ..
  docker compose up -d --no-build
  ```
- 没有 sandbox：不设置 `PERI_SWARM_BUN_URL`，`type=none` 正常，
  `openai_hosted` 创建时失败。
