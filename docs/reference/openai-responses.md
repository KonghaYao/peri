# OpenAI Responses 与 ChatGPT 计划登录

本文是操作参考，不是架构权威。边界见 [model-adapters](../design/model-adapters.md)
与 [configuration-authority](../design/configuration-authority.md)；配置定义由
`peri-config/src/app.rs`、`provider.rs` 维护。

## API key

在选中的 settings 文件中声明 `type: "openai_responses"`。旧 `type: "openai"`
继续使用 Chat Completions。`baseUrl` 是完整 Responses endpoint；省略时使用
`https://api.openai.com/v1/responses`。API key 可指向兼容的 HTTP(S) 服务。

下面的模型名、密钥和窗口数字是示例，请替换为所选模型的实际值。Peri 不按模型名
猜测窗口；每个使用 Responses 的 profile 都必须声明正整数 `contextWindow`。

```json
{
  "config": {
    "active_alias": "opus",
    "providers": [{
      "id": "responses",
      "type": "openai_responses",
      "baseUrl": "https://api.openai.com/v1/responses",
      "apiKey": "REPLACE_WITH_API_KEY",
      "auth": { "kind": "api_key" }
    }],
    "profiles": {
      "opus": { "provider": "responses", "model": "REPLACE_WITH_MODEL_ID", "effort": "high", "max_tokens": 4096, "contextWindow": 128000 },
      "fable": { "provider": "responses", "model": "REPLACE_WITH_MODEL_ID", "effort": "high", "max_tokens": 4096, "contextWindow": 128000 },
      "sonnet": { "provider": "responses", "model": "REPLACE_WITH_MODEL_ID", "effort": "medium", "max_tokens": 4096, "contextWindow": 128000 },
      "haiku": { "provider": "responses", "model": "REPLACE_WITH_MODEL_ID", "effort": "low", "max_tokens": 2048, "contextWindow": 128000 }
    }
  }
}
```

`effort` 和 `max_tokens` 继续由 profile 管理。Responses 不读取旧 `context_1m`
开关；主模型、摘要和子模型工厂都消费同一已解析配置。切换别名时会重新使用目标
profile 的模型、推理强度和窗口。

## ChatGPT 计划

```sh
peri --config-file /absolute/path/settings.json auth chatgpt login --account work
peri --config-file /absolute/path/settings.json auth chatgpt status --account work
peri --config-file /absolute/path/settings.json auth chatgpt logout --account work
```

登录会打印浏览器授权 URL，等待本机回调，并输出安全的账户状态与可复制的 `auth`
配置。浏览器中选择账户并授权；Ctrl-C 取消等待。凭据目录默认位于选中 settings
文件同目录的 `credentials/chatgpt`，也可对每条命令传 `--store /absolute/path`。
运行在其他机器的 ACP 宿主必须能访问其部署所选的凭据能力。

将 API key 示例 provider 的 `apiKey` 和 `auth` 替换为登录输出的引用：

```json
{
  "id": "responses",
  "type": "openai_responses",
  "auth": {
    "kind": "chatgpt_plan",
    "store": "/absolute/path/credentials/chatgpt",
    "account": "work",
    "subject": "COPY_FROM_LOGIN_OUTPUT",
    "clientId": "COPY_FROM_LOGIN_OUTPUT"
  }
}
```

此模式要求获准使用 ChatGPT plan 的 OAuth grant；仅拥有 ChatGPT/Codex 订阅不保证
当前账户或模型已获该 grant。Peri 使用授权服务器签发的 client ID 与 scope，不导入
`~/.codex/auth.json`，也不把旧 Codex token 发送到公共 API。缺少登录、grant 不包含
所需权限、身份不匹配或刷新失败都会返回错误，不会切换为 API key 计费。

`subject` 与 `clientId` 是非 secret 身份引用，每次取凭据和刷新都会核对它们。
本地账户别名登记身份后保持绑定，注销也保留注册信息；更换真实账户时使用新的
`--account` 别名，并复制新登录输出的引用。换身份或模型时，旧 Responses opaque
state 不会跨绑定重放；可用的通用消息仍按适配器契约处理。

ChatGPT plan profile 不发送 `max_output_tokens` 或 `temperature`；`max_tokens`
不会成为该模式的服务器输出上限，预算层把它标记为未知。`contextWindow` 仍需明确
配置。授权、凭据文件、刷新与注销均由原生 credentials MCP 能力负责。当前原生凭据
存储仅支持 Unix 的 owner-only 文件权限；Windows DACL 尚未实现，会明确返回不支持。
WASM 不提供原生浏览器登录或凭据文件能力。

注销先清除本地 grant，并尝试撤销远端授权，输出分别报告两项结果。同一个认证 manager
可以立即取消其请求；独立 CLI 进程不能立即取消另一进程已经在途的 HTTP，但其后续取
token 会失败。

工具名限 1–64 个 ASCII 字符，使用字母、数字、下划线或连字符。Responses opaque state
用于保留不可拆分的推理/调用顺序；本地预算对每段采用保守的 1024 token 估算，这不表示
服务器计费或实际占用上限。包含 opaque state 的工具交换不会进行可能破坏闭包的 Micro
省略，需要压缩时转用 Full 摘要。

现有 Login 面板能显示与保留 Responses provider；新增 Responses 配置和账户绑定
使用上述 CLI 与 settings。不要把 access token、refresh token 或 Codex 凭据粘贴进
`apiKey` 字段。
