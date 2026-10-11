# TUI 显示覆盖矩阵

> 本页只做有限枚举的反向核对，源头是当前 `TuiRenderUnit`、`PanelKind`、`PopupKind` 和顶层可见区域。目标交互以 [TUI 设计](../../design/tui-chat-workbench.md) 为准，具体显示仍须经真实终端执行用例验证；矩阵本身不是通过证据。

## 消息区渲染单元

| 当前枚举 | 对应用例 |
| --- | --- |
| `TuiUserBubble` | P0-07、P0-45、P0-56 |
| `TuiAssistantBubble` | P0-45、P0-50、P0-53 |
| `TuiToolCard` | P0-46、P0-51 |
| `TuiSystemNote` | P0-35、P0-48 |
| `TuiSystemReminder` | P0-48 |
| `TuiSubAgentGroup` | P0-18、P0-47 |
| `TuiCollapsedGroup` | P0-47 |
| `TuiDivider` | P0-48 |
| `TuiAskUserBlock` | P0-49、P0-65 |
| `TuiTodoSummary` | P0-48 |

`TuiRenderUnit` 当前共 10 个变体；子任务详情和 shell 详情是独立 surface，见 P0-18、P0-54。消息区滚动、选择、复制和 resize 见 P0-28、P0-51、P0-52。
工具卡的 `Generic`、`Skill`、`Todo` 三种 presentation 分别由 P1-24、P1-23 覆盖；`Streaming`、`Block`、`None` 三种流式模式由 P1-25 对照。

## PanelKind

| 当前枚举 | 对应用例 |
| --- | --- |
| `Model`、`Login`、`Config`、`Theme` | P0-61、P1-12、P1-13 |
| `Agent`、`Tasks`、`SubAgentDetail`、`ShellDetail` | P0-18、P0-54、P0-59、P1-12 |
| `ThreadBrowser` | P0-58、P1-12 |
| `Mcp`、`Plugin`、`Hooks`、`Cron` | P0-60、P0-44、P1-12、P1-14 |
| `Workflow` | P1-12、P1-14 |
| `AskUser` | P0-65、P1-12 |
| `Status`、`Memory`、`Goal` | P1-12、P1-15 |

`PanelKind` 当前共 18 个变体。P1-12 是逐项打开与关闭的目录巡检；其他用例核对身份、状态和失败，不能由目录巡检替代。

## PopupKind

| 当前枚举 | 对应用例 |
| --- | --- |
| `Hitl` | P0-24、P0-62 |
| `AskUser` | 旧枚举仍存在，但 overlay 当前渲染为空；现行交互见 `PanelKind::AskUser`、P0-65 |
| `Rewind`、`Confirm` | P0-64 |
| `OAuth` | P0-63 |
| `Download` | P1-16 |
| `ModelQuickSwitch` | P1-17 |

`PopupKind` 当前共 7 个变体。旧 AskUser popup 文件的存在不等于生产入口仍显示该弹窗。

## 其他可见区域与状态

| 区域或状态 | 对应用例 |
| --- | --- |
| Welcome / 首次配置 | P0-01、P1-08 |
| InputArea 多行、图片、历史、预测、补全 | P0-57、P1-09、P1-10 |
| StatusBar / footer 的准备、运行、终态、模型与权限 | P0-55、P1-11 |
| 待发送区与操作按钮 | P0-56、P2-01 |
| BgTaskArea 与 shell 详情 | P0-54 |
| 主区与详情滚动条、浏览态、窄屏、覆盖层 | P0-28～P0-31、P0-62～P0-67 |
| 主题、语言、Unicode、终端控制字符 | P0-52、P1-18 |
| 图片覆盖层、footer KeepGoing、通知与快捷提示 | P1-19～P1-21 |
| Composer 资源线、会话标题与附件计数 | P1-22 |

## 顶层可见组件

| 源码组件 | 对应用例 |
| --- | --- |
| `AppShell`、`SessionColumn`、`Welcome`、`SetupWizard` | P0-01、P0-31、P1-08 |
| `MessageArea`、`InputArea`、`StatusBar`、`BgTaskArea` | P0-45～P0-57、P1-20～P1-22 |
| `SteerQueue`、`SlashCompletion`、`MentionPopup` | P0-56、P1-10、P2-01 |
| `ImageOverlay`、`PanelOverlay`、`PopupOverlay` | P0-30、P1-19、P0-62～P0-65 |

## 对抗核对口径

1. 从源码枚举出发寻找无用例的变体：上述 10 个渲染单元、18 个面板和 7 个 popup 均有映射；AskUser 的旧空渲染路由被单独标明。
2. 从状态反查用例：空态、加载、实时、成功、失败、取消、历史重放、窄屏和切换会话均有显示路径；状态组合与未来新增变体不在本次有限枚举内。
3. 从交互反查结果：键鼠、焦点、滚动、复制、截断、详情、弹窗遮罩和主题失效都有明确的可观察验收；执行后仍须保留终端证据，不能把此矩阵当作测试通过。
