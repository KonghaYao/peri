# 首次配置向导

按 [总目录](README.md) 的编号执行。本区域在隔离 HOME 的首次启动路径上，核对向导各步的屏幕状态与最终落盘配置。

1. **P0-81 向导预设填充与保存**
   1. 以隔离 HOME（无既有配置与迁移来源）启动 TUI，进入首次配置向导。本用例只核对屏幕与落盘配置，向导不发起模型请求。
   2. 语言页按 Enter 确认；在 Choose 页用 ↑/↓ 选到「供应商预设」后 Enter；在预设页上下键逐项移动，确认每行形如「名称 + 摘要」且高亮随按键移动，对其中一家按 Enter；在随即打开的 Provider 编辑页确认已预填 provider_id、base_url 与四档模型，且光标停在 API Key；填入 Key，下移到「确认」并 Enter 回到列表页，确认该项已勾选后选中「提交」并 Enter，再在确认页提交保存。
   3. 核对编辑页与确认页的屏幕文本同 `$HOME/.peri/settings.json` 中该 provider 的 id、type、baseUrl、models 四档与 apiKey 逐字一致，预设值未被静默改写；不带 Key 的预设（apiKey 为空的厂商）在填入 Key 前按「确认」不能离开编辑页，列表页提交时报出缺少完整 ID、API Key 或模型。

2. **P0-81b 已有配置重开与 ChatGPT 订阅入口**
   1. 在已有可用 provider 的隔离 HOME 中启动，执行 `/setup`；确认直接进入 Provider 列表/编辑页（Form），而不是强制先选配置来源。
   2. 按 Esc 回到 Choose，确认仍可见「从 Claude Code 迁移」与「使用 ChatGPT 订阅登录」；选择订阅后先进入引导页：↑/↓、Tab 可切换「继续前往 Login」与「返回来源选择」，Esc 或点击返回回到 Choose，不改配置；确认继续后才进入与 `/login` 相同的 Provider 编辑和认证页面，可登录、打开浏览器或复制链接；取消登录回到 Choose。本用例不完成真实登录。
   3. 返回 Choose 选择「手动输入」；确认既有 provider 的 id、type、apiKey、baseUrl 与四档 models 未被默认值替换，保存后 `$HOME/.peri/settings.json` 中同一 id 不出现重复 provider。
   4. 另外准备一个无关 provider，走完 ChatGPT 保存路径（真实登录或测试替身），确认保存成功后进入与 `/model` 相同的档位编辑；填入 Provider、Model 与上下文窗口，Esc 完成首次设置，确认无关 provider 的字段与全局配置基线未被改写。
