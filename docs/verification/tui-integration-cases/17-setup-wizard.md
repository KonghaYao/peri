# 首次配置向导

按 [总目录](README.md) 的编号执行。本区域在隔离 HOME 的首次启动路径上，核对向导各步的屏幕状态与最终落盘配置。

1. **P0-81 向导预设填充与保存**
   1. 以隔离 HOME（无既有配置与迁移来源）启动 TUI，进入首次配置向导。本用例只核对屏幕与落盘配置，向导不发起模型请求。
   2. 语言页按 Enter 确认；在 Choose 页用 ↑/↓ 选到「供应商预设」后 Enter；在预设页上下键逐项移动，确认每行形如「名称 + 摘要」且高亮随按键移动，对其中一家按 Enter；在随即打开的 Provider 编辑页确认已预填 provider_id、base_url 与四档模型，且光标停在 API Key；填入 Key，下移到「确认」并 Enter 回到列表页，确认该项已勾选后选中「提交」并 Enter，再在确认页提交保存。
   3. 核对编辑页与确认页的屏幕文本同 `$HOME/.peri/settings.json` 中该 provider 的 id、type、baseUrl、models 四档与 apiKey 逐字一致，预设值未被静默改写；不带 Key 的预设（apiKey 为空的厂商）在填入 Key 前按「确认」不能离开编辑页，列表页提交时报出缺少完整 ID、API Key 或模型。
