# 配置与能力开关

按 [总目录](README.md) 的编号执行。使用隔离配置与会话，避免把宿主启动配置和当前会话数据混为一谈。

1. **P0-44 Plugin 失败与迟到结果不串会话**（[issue](../../../spec/issues/2026-10-06-p0-tui-architecture-optimization.md)）
   1. 在 Plugin 面板发起搜索或操作，依次制造服务端失败与断连。
   2. 切换会话后送达旧结果，再重试当前会话的请求。
   3. 核对失败有明确反馈、旧结果不覆盖新会话；键鼠提交相同语义请求。
2. **P0-83 仅用 TUI 创建 Responses API key provider**
   1. 使用隔离配置并保留至少一个既有 provider 作为基线（空配置的首 provider 会让四档
      都走首 provider 回退，需四档各自窗口，另行核对），打开 `/login`，Ctrl+N 新建
      provider，用 Left/Right/Space 选择 `OpenAI Responses`，认证保持 `API key`，填写唯一
      名称、Base URL、测试 Key 与 provider 四档默认 model。
   2. 在 `Profile configuration` 只勾选 active 档位，填写该档 model 和正整数
      `Context window (tokens)`；确认其余三档未勾选，按 Ctrl+S 保存。
   3. 核对提示为保存成功；读取实际 settings，确认 provider 类型、API key auth、该档
      provider/model/contextWindow 已写入，未勾选三档的逐字段值未变化；重启或重开
      `/login`、`/model` 后显示与磁盘一致。全程不执行 CLI、不手改 settings。
3. **P0-84 已有 provider 类型防误改与四档隔离**
   1. 准备已有 Responses provider 与四个不同 profile，进入 `/login` 编辑态；核对默认焦点在
      Provider ID，在类型行按 Left/Right/Space，再按 Enter 打开类型确认并只按 Esc。
   2. 选择非 active 档，记录 active alias、其他档 effort/model/window；只修改该档
      window 后保存；再次编辑并重命名 provider，保存。
   3. 核对类型未被方向键或确认取消改变；active alias 未切换，未编辑档逐字段不变；
      provider 改名后所有原显式引用仍有效，重开面板显示新名称与原绑定。
4. **P0-85 无效窗口、空模型与 revision 冲突都不落盘**
   1. 在 `/login` 逐个输入 `0`、`-1`、`128k`、超出 u32 的长数字，并粘贴含换行内容，
      另一次清空 profile model，分别尝试保存。
   2. 每次核对 inline error 与焦点落在对应字段，通知可见；普通字段/输入错误不显示
      “重新载入”提示；读取磁盘、内存配置与 ACP 帧，确认没有部分写入或运行端更新。
   3. 保持草稿打开，由另一个 Peri 进程经配置源写入（或直接编辑 settings）产生新 revision，
      再保存旧草稿；核对显示 revision 冲突（只有此时出现重新载入提示）、草稿保留、外部
      新值未被覆盖；按 Ctrl+R 从 authority 重读并按最新 revision 重新基线化，填写有效值
      后可保存；另在 Esc 重开 `/login` 后核对面板打开即载入新值并可保存修正结果。

## P1 补充场景

1. **P1-04 Beta 开关只对新会话生效**（[issue](../../../spec/issues/2026-10-08-beta-flags-implementation.md)）
   1. 在 Config 面板开启 full-async-tools，保存后保留当前会话并新建会话。
   2. 分别在两个会话调用未显式指定后台模式的 Bash 或 Agent。
   3. 核对旧会话仍用原行为、新会话默认后台；显式 false 仍前台，重启后设置保留。
