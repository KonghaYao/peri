# 配置与能力开关

按 [总目录](README.md) 的编号执行。使用隔离配置与会话，避免把宿主启动配置和当前会话数据混为一谈。

1. **P0-44 Plugin 失败与迟到结果不串会话**（[issue](../../../spec/issues/2026-10-06-p0-tui-architecture-optimization.md)）
   1. 在 Plugin 面板发起搜索或操作，依次制造服务端失败与断连。
   2. 切换会话后送达旧结果，再重试当前会话的请求。
   3. 核对失败有明确反馈、旧结果不覆盖新会话；键鼠提交相同语义请求。
2. **P0-83 仅用 TUI 创建 Responses API key provider**
   1. 使用隔离配置并保留至少一个既有 provider 作为基线，打开 `/login`，Ctrl+N 新建
      provider，用 Left/Right/Space 选择 `OpenAI Responses`，认证保持 `API key`，填写唯一
      名称、Base URL、测试 Key 与 provider 四档默认 model。另新建一个 `ChatGPT plan`
      provider：切到 plan 认证后核对四档默认 model 已从已保存的同类 plan provider（没有则
      其他 Responses provider）预填，手动改写的字段不被后续触发覆盖；若没有可复用来源，
      核对保留未设置并出现可操作提示。
   2. 核对 Edit 中不出现“档位配置/编辑档位/绑定此 Provider/此档模型/上下文窗口”；新建
      provider 不要求选择档位或填写 contextWindow，按 Ctrl+S 保存。
   3. 核对提示为保存成功；读取实际 settings，确认 provider 类型、API key auth 与四档默认
      model 已写入（plan provider 的预填模型也应落盘），四个 profile 逐字段与保存前完全一致
      （没有新增/改写绑定、model 或 contextWindow）。重启或重开 `/login` 后显示与磁盘一致。
   4. 打开 `/model`，为目标档选择新 provider，确认 Provider/Model 立即保存且右栏始终是
      同一张 K/V 表；把光标移到 Responses 档位的 Context window 行直接输入并 Enter 提交，
      确认窗口写入；再重启核对一致。未绑定档位继续使用已有的首 provider 回退，
      不因 Login 保存被写死。
   5. 视觉核对：各分区标题、标签列与值列对齐；80/100 列下长 Base URL 不被硬裁切；
      `Type` 的锁定提示落在字段续行，点击续行仍聚焦类型行。
3. **P0-84 已有 provider 类型防误改与绑定保持**
   1. 准备已有 Responses provider 与四个不同 profile，进入 `/login` 编辑态；核对默认焦点在
      Provider ID，在类型行按 Left/Right/Space，再按 Enter 打开类型确认并只按 Esc。
   2. 记录 active alias 与四档的 provider/model/contextWindow/effort；只修改 provider 的
      Base URL 或默认 model 后保存；再次编辑并重命名 provider，保存。
   3. 核对类型未被方向键或确认取消改变；active alias 未切换；四个 profile 的绑定与参数
      逐字段不变，只有显式引用随 provider 改名更新；未绑定档位的空引用仍为空。重开面板
      显示新名称与原绑定。
   4. 对已有 ChatGPT plan provider 核对：Edit/认证详情/注销确认/登录状态/错误提示均不出现
      账号引用、身份与 Client ID；凭据目录只在“高级”区只读显示且长路径中间截断。保存后
      读取 settings，确认 `auth.store/account/subject/clientId` 仍完整落盘。
   5. 把凭据目录改为相对路径后保存：核对“高级”区恢复为可编辑行并聚焦，修正后保存成功；
      若保存错误指向账号引用，焦点落到认证动作组而不是不可见字段。
   6. 认证动作核对：四个动作在同一行横向排列，`←/→` 选择、`Enter` 执行；鼠标点击不同动作
      标签执行各自动作，点击动作间隔/标记列不执行；窄屏仍保持一行且选中动作可见。
   7. 把已有 Responses provider 的类型改为非 Responses，再改回 Responses：核对既有 profile
      绑定与参数在两次保存前后都不变。若某绑定原本可解析、本次因清空默认模型或改变类型
      变为不可解析，保存被阻断：模型缺失聚焦对应 Login 默认模型字段，窗口缺失/绑定不可解析
      指向 Model 面板；若绑定原本就不可解析且本次仍未修好，保存成功并给出非阻断提示，
      不自动补绑定。
4. **P0-85 Login 保存边界与 revision 冲突**
   1. 在 `/login` 触发字段级错误（空名称、重复名称、非法 Base URL）与多行粘贴清洗，
      分别尝试保存；核对 inline error 与焦点落在对应 provider 字段，通知可见；普通字段/
      输入错误不显示“重新载入”提示；读取磁盘、内存配置与 ACP 帧，确认没有部分写入。
   2. 构造一个已有 Responses 绑定缺少 `contextWindow` 的配置，在 Login 保存 provider；核对
      保存成功并提示该档位此前就无法解析（非阻断），profiles 未被自动改写。再到 `/model`
      补齐后回 Login 保存，核对成功且其他字段不变。另构造 baseline 可解析、本次清空该档
      默认模型的草稿：核对保存被阻断，错误指向对应 Login 默认模型字段而非笼统的 Model 面板。
   3. 已登录且作用域一致、草稿身份为空时直接保存新建 plan provider：核对一次保存成功、
      无“请先登录”红字，settings 含 `auth.store/account/subject/clientId` 与四档默认模型，
      profiles 与 active alias 逐字段不变。`Checking` 期间保存只提示等待、不写红字；状态查询
      成功后再次保存成功。已有不同身份时保存被阻断，默认选中“使用此账号”，采用后保存成功。
   4. 保持草稿打开，由另一个 Peri 进程经配置源写入（或直接编辑 settings）产生新 revision，
      再保存旧草稿；核对显示 revision 冲突（只有此时出现重新载入提示）、草稿保留、外部
      新值未被覆盖；按 Ctrl+R 从 authority 重读并按最新 revision 重新基线化，可保存；
      另在 Esc 重开 `/login` 后核对面板打开即载入新值并可保存修正结果。

## P1 补充场景

1. **P1-04 Beta 开关只对新会话生效**（[issue](../../../spec/issues/2026-10-08-beta-flags-implementation.md)）
   1. 在 Config 面板开启 full-async-tools，保存后保留当前会话并新建会话。
   2. 分别在两个会话调用未显式指定后台模式的 Bash 或 Agent。
   3. 核对旧会话仍用原行为、新会话默认后台；显式 false 仍前台，重启后设置保留。
