# OpenAI Responses 与 ChatGPT 计划登录

本文是操作参考，不是架构权威。边界见 [model-adapters](../design/model-adapters.md)
与 [configuration-authority](../design/configuration-authority.md)；配置定义由
`peri-config/src/app.rs`、`provider.rs` 维护。

## 在 TUI 中配置

常规本机使用不需要手工编辑 settings 或执行 CLI：

1. 打开 `/login`（面板打开时会从配置 authority 重读最新配置），Ctrl+N 新建 provider
   （或在列表选中已有 Responses provider 后 Enter；编辑已有 provider 时焦点默认在
   Provider ID）。
2. 新建时用 Left/Right/Space 选择 `OpenAI Responses`；已有 provider 的类型在编辑态锁定，
   只有 Enter 打开类型确认、再按 Enter 才改变。
3. 选择认证方式：`API key` 填写 Base URL 与 Key；`ChatGPT plan` 填写凭据目录与账号引用，
   再按 `Sign in to ChatGPT` 完成浏览器授权。新建 Responses + ChatGPT plan provider 时，
   四档默认模型会从已保存的同类 plan provider（其次其他 Responses provider）预填；只填充
   空字段，手动输入优先，没有可靠来源时保留未设置并给出提示。预填只写 Login 草稿，不写
   任何 profile。
4. Login 只保存 provider：档位绑定、该档 model 与正整数 `Context window (tokens)` 归
   settings 的 `profiles`（`/model` 面板只编辑 Provider/Model/Effort/Max tokens/1m，
   不提供 Responses 专属字段）。新建 provider 不自动绑定任何档位，已有 provider 的绑定与其参数保持
   不变；改名时所有显式引用随新 id 更新，空引用继续走首 provider 回退。保存只阻断“本次
   新弄坏”的已有绑定：原本就可解析、本次变为不可解析时显示精确错误（模型缺失指向 Login
   默认模型字段，窗口缺失指向 settings 的 `profile.contextWindow`；Model 面板不提供
   Responses 专属字段）；原本就不可解析的绑定允许保存并给非阻断提示，不会自动补绑定或
   清空引用。
5. 已登录且作用域一致时，空/兼容的 plan 身份会在状态查询成功或保存门自动采用，一次保存
   即可完成；已有不同身份必须显式选择“使用此账号”，不会自动覆盖。`Checking` 期间保存只
   提示等待，不写红字。`SignedOut`/查询失败/作用域变化各有对应文案与推荐动作。
6. 按 Ctrl+S 或 `Confirm` 保存。字段错误（含多行粘贴被清洗的输入）会阻断保存并只显示
   对应提示；认证状态变化会清除陈旧的身份/引用错误。长草稿使用进入表单时的配置 revision，
   出现 revision 冲突时按 Ctrl+R 从 authority 重读并重新基线化（同时同步认证作用域），
   不会用旧草稿覆盖外部新值，只有该冲突显示重新载入提示。
7. 在 `/model` 右栏选择要绑定的档位并编辑 Provider / Model / Effort / Max tokens /
   1m enable：所有 provider（含 Responses / ChatGPT plan）共用同一张 K/V 表与即时保存
   路径，不出现档位草稿、专属保存面板或 Responses 专属字段。Login 落盘的四档默认模型
   会用于 Model 面板预填；已有显式 `profile.model` 优先，不会被预填覆盖。底部
   `[Ctrl+L 管理 Provider · 认证：/login]` 是按钮，点击与 Ctrl+L 相同：打开 Login
   并定向编辑当前 provider。
8. Login Edit 的字段、类型/认证方式/Confirm 可用鼠标点击，语义与键盘一致。Model 右栏
   Provider/Model/Effort/Max tokens/1m enable 行也可点击：先聚焦该行，再执行与键盘
   相同的动作。空白、边框、分隔线与滚动条不触发动作；右栏滚动后点击可见行仍命中该字段。
9. 已确认的 ChatGPT plan 模型缺窗口时按 1M 默认值立即可用；其它 Responses 模型
   缺窗口的绑定可保存为未完成状态，但激活前须填写正整数窗口（其它档位的编辑不受影响）。

ChatGPT 登录、状态查询与注销都在 Login 面板内完成，不启动 `peri auth chatgpt` 子进程，
也不读取或显示 token。登录成功后凭据已保存，但 provider 配置仍需按上一步保存；两者不是
一个事务。CLI 命令保留给无 TUI、脚本化或排障场景。

## API key

在选中的 settings 文件中声明 `type: "openai_responses"`。旧 `type: "openai"`
继续使用 Chat Completions。`baseUrl` 是完整 Responses endpoint；省略时使用
`https://api.openai.com/v1/responses`。API key 可指向兼容的 HTTP(S) 服务。

下面的模型名、密钥和窗口数字是示例，请替换为所选模型的实际值。Peri 不按模型名
猜测窗口；API key 模式及未确认的订阅模型仍须声明正整数 `contextWindow`。ChatGPT plan
绑定 `gpt-6.1-sol`、`gpt-6.1-terra`、`gpt-6-luna`、`gpt-6-astra` 时，未设置窗口默认按
1,000,000 解析并在 Model 面板显示，不写回配置；显式设置的正整数优先，显式零仍无效。
其他未填窗口的绑定可以保存（作为未完成绑定），但补齐前不能解析为运行 provider。

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

流式 `response.metadata` 和 `codex.response.metadata` 只承载元数据，不改变文本或完成态；
若附带 response 身份，仍须匹配当前响应。允许 `response.output_item.done` 独立携带
完整 item（消息可缺少 item ID，原生回放不伪造 ID）；`response.completed` 未提供
`output` 或省略此前已发送的 item 时，仅以已收到完整 `output_item.done` 的快照
重建结果和 replay。未完成的 item、冲突快照及
未知的输出事件继续报协议错误；不产生输出的扩展事件忽略。失败终态仍报错。若上游明确
给出 `end_turn: false` 且没有工具调用，该响应不触发完成 hook，而是有界续跑。

## ChatGPT 计划

以下 CLI 命令是无 TUI、脚本化或排障时的替代路径：

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
不会成为该模式的服务器输出上限，预算层把它标记为未知。Model 面板仍与其它 Responses
provider 一样允许编辑 Provider/Model/contextWindow/Max tokens；是否被服务端采用属于
运行时可用性，不改变配置行为。`contextWindow` 仍需明确配置。授权、凭据文件、刷新与注销
均由原生 credentials MCP 能力负责。

注销先清除本地 grant，并尝试撤销远端授权，输出分别报告两项结果。同一进程内的认证
manager 可以取消等待；独立 CLI 进程不能立即取消另一进程已经在途的 HTTP，但其后续取
token 会失败。

工具名限 1–64 个 ASCII 字符，使用字母、数字、下划线或连字符。Responses opaque state
用于保留不可拆分的推理/调用顺序；本地预算对每段采用保守的 1024 token 估算，这不表示
服务器计费或实际占用上限。包含 opaque state 的工具交换不会进行可能破坏闭包的 Micro
省略，需要压缩时转用 Full 摘要。

TUI 的 Login 面板可完成 Responses provider 创建、API key / ChatGPT plan 认证选择、四档
默认 model 映射以及登录、状态与注销；档位绑定与 `contextWindow` 由 `/model` 面板配置。
新建 plan provider 的四档默认 model 会从已保存的同类 plan provider（其次其他 Responses
provider）预填，只填空字段且不覆盖手动输入；没有来源时不伪造模型名。Login 保存不会写入
或改写任何 profile：新建 provider 不自动绑定，已有 provider 的绑定与参数保持不变。平台与
远程环境仍有明确限制：

- 原生凭据存储目前仅支持 Unix；Windows DACL 尚未实现，会返回不支持，API key 模式仍可用。
  WASM 不提供原生浏览器登录或凭据文件能力。
- 浏览器授权回调固定在本机 `127.0.0.1` 随机端口，等待上限 600 秒。SSH、容器或其他机器
  上的 TUI 需要端口转发才能完成回调；仅复制授权 URL 到另一台机器不能保证回调可达。
- 凭据保存与 provider 配置保存是两阶段：登录成功后凭据已落盘，配置仍可能保存失败；界面
  会明确提示，重试保存不会重跑 OAuth。取消登录等待只停止本地等待，结果未确认时按未知
  处理并可再次查询状态。
- `status` 读取本地凭据与身份绑定，不代表实时 inference 已成功；已登录且作用域一致时，
  空/兼容身份会自动采用，已有不同身份必须显式选择“Use this account”才写入引用。
- 状态中的到期时间是 access token 的过期时间，不是登录授权失效：使用时会自动刷新，
  只有刷新失败才需要重新登录。过期状态不阻断 provider 配置保存。
- 注销先清除本地 grant，再尝试远端撤销；远端失败会如实显示“未确认”，本地清理不会因
  取消等待而跳过。注销不会删除 provider 配置或整个凭据目录。

不要把 access token、refresh token 或 Codex 凭据粘贴进 `apiKey` 字段。
