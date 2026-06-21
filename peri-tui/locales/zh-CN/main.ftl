# ============================================================
# Peri TUI — 简体中文 (zh-CN) Translation File
# Key names must match en/main.ftl exactly.
# ============================================================

# ---- i18n 基础设施测试 key ----
test-hello = 你好，世界！
test-greeting = 你好，{ $name }！
ui-empty = 无

# ---- Command Descriptions ----

command-help-description = 列出所有可用命令
command-clear-description = 清空消息列表
command-exit-description = 退出应用
command-compact-description = 压缩对话上下文（结构化摘要 + 重新注入最近文件/Skills）
command-model-description = 打开模型选择面板（Provider + 级别 + Thinking）；带参数时直接切换别名（opus/sonnet/haiku）
command-login-description = 管理 Provider 配置（新建/编辑/删除）
command-cost-description = 查看当前会话费用和 token 消耗
command-context-description = 查看上下文使用率和会话统计
command-agents-description = 打开 Agent 选择面板
command-mcp-description = 管理 MCP 服务器连接
command-memory-description = 编辑用户/项目级 CLAUDE.md 记忆文件
command-history-description = 打开历史对话浏览面板
command-loop-description = 注册定时循环任务（自然语言描述，如 /loop 每隔5分钟提醒我喝水）
command-cron-description = 查看和管理定时任务
command-tasks-description = 查看 agent 线程和定时任务
command-plugin-description = 管理插件（浏览、安装、卸载）
command-config-description = 全局配置（autocompact、语言、系统提示词覆盖）
command-hooks-description = 查看 Hook 配置
command-effort-description = 查看或设置推理力度（low/medium/high/xhigh/max）
command-rename-description = 查看或修改当前会话标题
command-lang-description = 切换界面语言（如 /lang zh-CN）
command-setup-description = 打开配置向导，设置 Provider
command-agent-description = 设置 Agent 定义，切换不同的 Agent 角色

# ---- Command Execution Messages ----

# help command
help-available-commands = 可用命令：
help-alias-prefix = （别名: /{ $aliases }）
help-skills-count = Skills（{ $count } 个可用）: 输入 # 前缀查看
help-skills-empty = Skills: 将 .md 文件放入 .claude/skills/ 目录即可添加
help-shortcuts = 快捷键：Shift+Tab 切换权限模式 │ { $model_key } 切换模型 │ Shift+Enter 换行 │ Esc 退出 │ Ctrl+C 中断

# compact command
compact-agent-running = Agent 运行中，无法执行压缩

# history command
history-agent-running = Agent 运行中，无法打开历史面板

# model command
config-save-failed = 配置保存失败: { $error }

# effort command
effort-set = 推理力度已设为 { $effort }
effort-current = 当前推理力度: { $effort }
effort-usage = 用法: /effort low|medium|high|xhigh|max

# loop command
loop-usage = 用法: /loop <自然语言时间描述> <提示词>
loop-example = 例如: /loop 每隔5分钟提醒我喝水

# rename command
rename-no-session = 当前无活跃会话，无法重命名
rename-current-title = 当前标题: { $title }
rename-updated = 会话标题已更新为: { $name }
rename-failed = 重命名失败: { $error }
rename-untitled = （无标题）

# lang command
lang-switched = 语言已切换为 { $lang }
lang-available = 可用语言: { $langs }
lang-unsupported = 不支持的语言: { $lang }

# ---- Status Bar ----

statusbar-permission-dont-ask = Don't Ask
statusbar-permission-accept-edit = Accept Edit
statusbar-permission-auto = Auto Mode
statusbar-permission-bypass = Bypass
statusbar-copied = 已复制 { $count } 个字符
statusbar-no-agent = 无
statusbar-bg-indicator = [BG: { $count }]
statusbar-retrying = 重试 { $attempt }/{ $max } ({ $delay }s): { $error }
statusbar-mcp-connecting =  MCP ({ $connected }/{ $total })...
statusbar-mcp-ready =  MCP 就绪 ({ $total } 个服务器)
statusbar-mcp-failed =  MCP 失败: { $msg }
statusbar-lsp-diag = 诊断: { $errors }E/{ $warnings }W

# ---- Status Bar Shortcut Hints (main view) ----

key-command = 命令
key-switch-session = :切换Session
key-close = :关闭
key-scroll = :滚动
key-cancel = :取消
key-newline = :换行
key-open-browser = :打开浏览器
key-submit = :提交
key-switch = :切换
key-switch-tab = :切换标签
key-move = :移动
key-select = :选择
key-confirm = :确认
key-delete = :删除
key-reconnect = :重连
key-detail = :详情
key-execute = :执行
key-back = :返回
key-install = :安装
key-tab = :切换
key-effort = :力度
key-switch-model = :切换模型

# ---- Welcome Page ----

welcome-title = Peri Agent Framework
welcome-divider = ────── 我能做什么？ ──────
welcome-feature-code = 让我帮你编写、调试或重构代码
welcome-feature-files = 管理文件和运行终端命令
welcome-feature-agents = 将任务委派给专业子 Agent
welcome-login-hint-1 = 请输入
welcome-login-hint-2 = 配置 API Key 开始使用
welcome-shortcut-quit = :退出
welcome-shortcut-stop = :停止
welcome-shortcut-newline = :换行
welcome-shortcut-mode = :模式
welcome-shortcut-model = :模型
welcome-skills-available = { $count } 个 skills 可用

# ---- Tips (18 items) ----

tip-0 = 按 / 输入命令，Tab 补全
tip-1 = Ctrl+C 中断 Agent，Shift+Tab 切换权限模式
tip-2 = Ctrl+T 切换模型（opus / sonnet / haiku），Ctrl+Shift+T 切换 Provider
tip-3 = Shift+Enter 在输入框中换行
tip-4 = 拖拽文件或图片到终端可自动附加到消息
tip-5 = 长按 Ctrl+V 粘贴剪贴板图片
tip-6 = Ctrl+U/D 滚动消息历史，↑/↓ 浏览输入历史
tip-7 = Ctrl+N/P 切换 Session，Ctrl+W 关闭
tip-8 = Esc 关闭弹窗或面板，Enter 确认选择
tip-9 = /compact 压缩上下文节省 token
tip-10 = /clear 清空当前对话
tip-11 = /model 切换 LLM 模型
tip-12 = /history 浏览历史对话记录
tip-13 = /loop 创建定时循环任务
tip-14 = /plugin 管理 Claude Code 插件
tip-15 = 在 .claude/skills/ 中添加自定义 Skills
tip-16 = 在 .claude/agents/ 中定义 SubAgent
tip-17 = 对复杂任务让 Agent 先制定计划再执行

# ---- Setup Wizard ----

setup-welcome-title =  ── Peri 设置 ── 欢迎
setup-choose-provider =  选择如何配置你的 Provider：
setup-source-custom-api = Custom API
setup-source-migrate = 从 Claude Code 迁移
setup-source-custom-desc = 手动输入 Provider 详情
setup-source-migrate-desc = 从 ~/.claude/ 导入配置
setup-key-confirm = :确认
setup-key-select = :选择
setup-key-quit = :退出
setup-configure-title =  ── Peri 设置 ── 配置 Providers
setup-submit = 提交
setup-key-edit-submit = :编辑/提交
setup-key-check = :勾选
setup-key-back = :返回
setup-edit-title =  ── 设置 ── 编辑: { $type } ({ $id })
setup-field-type = 类型
setup-field-id = ID
setup-field-base-url = 基础URL
setup-hint-base-url-v1 = OpenAI Base URL 需要 /v1 后缀
setup-field-api-key = API密钥
setup-field-opus = 旗舰
setup-field-sonnet = 标准
setup-field-haiku = 极速
setup-model-label = Model
setup-label-key = 密钥：
setup-provider-anthropic = Anthropic
setup-provider-openai = OpenAI 兼容
setup-confirm = 确认
setup-test-connectivity = [ 测试联通性 ]
setup-key-switch-type = :切换类型
setup-key-back-list = :返回列表
setup-complete-title =  ── 设置完成 ✓
setup-press-enter = 按
setup-to-start = 开始使用
setup-no-key = (无密钥)
setup-no-providers = 未配置任何 Provider，请选择"Custom API"或从 Claude Code 导入。

setup-language-title = ── Peri 设置 ── 语言
setup-language-prompt = 选择界面语言：
setup-language-press-enter = 按 Enter 确认

# ---- Config Panel ----

config-panel-title =  /config — 配置
config-field-autocompact = Autocompact
config-field-compact-threshold = Compact 阈值
config-field-language = 语言
config-field-persona = Persona
config-field-tone = Tone
config-field-proactiveness = Proactiveness
config-field-cache-warning = 缓存警告
config-field-diff = 内联 Diff
config-value-on = 开
config-value-off = 关
config-saved = 配置已保存

# Config panel groups
config-group-general = 通用
config-group-prompt-overrides = 提示词覆盖

# Config field descriptions
config-desc-autocompact = （开/关 — 上下文满时自动压缩）
config-desc-threshold = 50-99% — 自动压缩触发阈值
config-desc-language = en, zh-CN，或留空为自动
config-desc-persona = 覆盖系统提示词 persona（留空=默认）
config-desc-tone = 覆盖系统提示词 tone（留空=默认）
config-desc-proactiveness = low / medium / high — agent 主动性级别
config-desc-cache-warning = （开/关 — 在对话中显示缓存命中率过低警告）
config-desc-diff = （开/关 — 显示 Write/Edit 工具的内联 diff）
config-field-streaming = 渲染模式
config-desc-streaming = streaming / block / none — LLM 输出渲染粒度

# ---- Login Panel ----

login-panel-title-browse =  /login — Provider 管理
login-panel-title-edit =  /login — 编辑 Provider
login-panel-title-new =  /login — 新建 Provider
login-panel-title-confirm-delete =  /login — 确认删除
login-no-model = （未设置）
login-empty-hint =   （无 provider，按 Ctrl+N 新建）
login-confirm-delete-label =  确认删除
login-confirm-delete-question =  ？
login-key-activate = :激活
login-key-new = :新建
login-key-delete = :删除
login-key-paste = :粘贴
login-confirm-delete = :确认删除

# ---- HITL Popup ----

hitl-single-title =  ⚠ 工具审批 (1 项)
hitl-batch-title =  ⚠ 批量工具审批
hitl-approved = [批准]
hitl-rejected = [拒绝]
hitl-summary = 已选: { $approved } 批准 / { $rejected } 拒绝

# ---- AskUser Popup ----

ask-user-placeholder = 输入自定义内容...

# ---- App Messages ----

app-provider-ready = { $name } ({ $model }) 已就绪
app-not-configured = 未配置
app-empty = 无
app-no-api-key-warning = 警告: 未设置任何 API Key（ANTHROPIC_API_KEY 或 OPENAI_API_KEY）
app-interrupted-resumed = 已强制中断
app-interrupt-done = 已中断
app-interrupted-background = 已强制中断
app-config-saved = 配置已保存
app-config-save-failed = 配置保存失败: { $error }
app-provider-activated = 已激活 Provider: { $name }
app-provider-created = 已新建并激活 Provider: { $name }
app-provider-saved = 已保存并激活 Provider: { $name }
app-provider-deleted = 已删除 Provider: { $name }
app-provider-name-empty = 保存失败：Provider 名称不能为空
app-agent-reset = Agent 已重置（未设置 agent_id）
app-agent-switched = Agent 已切换为: { $name } ({ $id })
app-agent-disconnected = Agent 连接异常断开，请重试发送消息
app-compact-no-context = 无可压缩的上下文（历史消息为空）
app-compact-no-provider = 压缩失败: 未配置 LLM Provider（请设置 ANTHROPIC_API_KEY 或 OPENAI_API_KEY）
app-compact-compressing = 压缩上下文
app-compact-done = 上下文已压缩
app-compact-failed = 压缩失败: { $error }
app-compact-auto-cleared = 自动清理：释放了 { $count } 个工具调用结果
app-compact-limit-reached = 上下文压缩后仍超出限制，已停止自动继续。请使用 /compact 手动压缩或 /clear 清空历史。
app-model-switched = 模型已切换为: { $alias } ({ $effort } effort)
app-1m-context-enabled = 已启用 1M 上下文模式（context window: 1,000,000 tokens）
app-prompt-cache-low = Prompt cache 命中率 { $rate }% < 80% (req: { $req })
app-no-mcp-configured = 无 MCP 服务器配置（请在 .mcp.json 或 settings.json 中添加）
app-no-cron-tasks = 无定时任务
app-cron-deleted = 已删除定时任务: { $preview }
app-submit-attachments = { $input } [ { $count } 张图片 ]
app-no-provider-submit = 未配置 API Key，请输入 /login 配置 Provider
app-bg-task-done = [后台任务 { $id } 已完成] Agent: { $agent } | 工具调用: { $tools } | 耗时: { $duration }ms
app-bg-task-done-with-result = [后台任务 { $id } 已完成] Agent: { $agent } | 工具调用: { $tools } | 耗时: { $duration }ms\n结果:\n{ $result }
app-bg-task-failed = [后台任务 { $id } 执行失败] Agent: { $agent } | { $error }
app-bg-task-failed-with-error = [后台任务 { $id } 执行失败] Agent: { $agent }\n错误:\n{ $error }
app-bg-continuation = 正在回顾 { $count } 个后台 Agent 结果...

# ---- Panel Status Bar Hints ----

# Login panel
hint-login-browse = :导航
hint-login-activate = :激活
hint-login-edit = :编辑
hint-login-new = :新建
hint-login-delete = :删除
hint-login-close = :关闭
hint-login-field = :字段
hint-login-save = :保存
hint-login-paste = :粘贴
hint-login-toggle = :切换
hint-login-back = :返回

# Config panel
hint-config-field = :字段
hint-config-toggle = :切换
hint-config-save = :保存并关闭

# Model panel
hint-model-navigate = :导航
hint-model-confirm = :确认
hint-model-effort = :Effort
hint-model-close = :关闭

# Agent panel
hint-agent-select = :选择
hint-agent-confirm = :确认
hint-agent-cancel = :取消

# MCP panel
hint-mcp-navigate = :导航
hint-mcp-detail = :详情
hint-mcp-reconnect = :重连
hint-mcp-delete = :删除
hint-mcp-execute = :执行
hint-mcp-back = :返回
hint-mcp-close = :关闭

# ---- MCP Panel Content ----

mcp-server-count = { $count } 个服务器
mcp-section-project = 项目 MCP
mcp-section-project-path = 项目 MCP ({ $path })
mcp-section-user = 用户 MCP
mcp-section-user-path = 用户 MCP ({ $path })
mcp-section-plugin = 插件 MCP
mcp-no-servers = 未配置 MCP 服务器。编辑 .mcp.json 或 settings.json
mcp-panel-title = 管理 MCP 服务器
# Status
mcp-status-connected = 已连接
mcp-status-needs-auth = 需要认证
mcp-status-error = 错误
mcp-status-disabled = 已禁用
mcp-status-uninitialized = 未初始化
mcp-status-offline = 离线
# Auth
mcp-auth-authenticated = 已认证
mcp-auth-none = 无
# Labels
mcp-label-status = 状态:
mcp-label-auth = 认证:
mcp-label-url = URL:
mcp-label-config-location = 配置位置:
mcp-label-plugin = 插件
mcp-label-plugin-source = 插件 - { $source }
mcp-label-capabilities = 能力:
mcp-label-tools = 工具:
mcp-label-tools-count = { $count } 个工具
# Capabilities
mcp-capability-tools = 工具
mcp-capability-resources = 资源
# Actions
mcp-action-hide-tools = 隐藏工具
mcp-action-view-tools = 查看工具
mcp-action-reauthenticate = 重新认证
mcp-action-clear-auth = 清除认证
mcp-action-reconnect = 重新连接
mcp-action-disable = 禁用
mcp-action-enable = 启用
# OAuth Messages
mcp-oauth-completed = [i] OAuth 授权完成: { $server }
mcp-oauth-failed = [i] OAuth 授权失败: { $server } - { $error }
mcp-clear-auth-ok = [i] OAuth 凭证已清除: { $server }
mcp-clear-auth-failed = [i] 清除 OAuth 凭证失败: { $server }
mcp-action-ok = [i] 操作完成: { $server }
mcp-action-failed = [i] 操作失败: { $server }

# Plugin panel
hint-plugin-uninstall = :确认卸载
hint-plugin-cancel = :取消
hint-plugin-delete = :确认删除
hint-plugin-add = :添加
hint-plugin-exit-search = :退出搜索
hint-plugin-tab = :Tab
hint-plugin-install = :安装
hint-plugin-remove = :Remove
hint-plugin-navigate = :导航
hint-plugin-execute = :执行
hint-plugin-back = :返回列表
hint-plugin-select = :选择
hint-plugin-search = :搜索

# Cron panel
hint-cron-confirm-delete = :确认删除
hint-cron-navigate = :导航
hint-cron-toggle = :切换
hint-cron-delete = :删除
hint-cron-close = :关闭

# Status panel
hint-status-tab = :切换Tab
hint-status-close = :关闭

# History panel
hint-history-confirm-delete = :确认删除
hint-history-exit-search = :退出搜索
hint-history-close = :关闭

# Hooks panel
hint-hooks-navigate = :导航
hint-hooks-close = :关闭

# Memory panel
hint-memory-select = :选择
hint-memory-edit = :编辑
hint-memory-close = :关闭

# ---- Plugin Panel Messages ----

app-plugin-updating = 正在更新 marketplace: { $name }
app-plugin-delete-failed = 删除失败: { $error }
app-plugin-add-failed = 添加失败: { $error }
app-plugin-added = Marketplace 已添加: { $name } (正在获取内容...)

# 后台 Agent 管理栏
bg-bar-focus-hint = 按 Esc 退出聚焦

# ---- 模型面板 ----

model-panel-title =  选择模型 
model-panel-description =   切换模型。仅对当前会话生效。
model-field-max-token = 最大 Token
model-field-effort = 推理力度
model-field-1m-context = 1M 上下文
model-effort-low = 低
model-effort-medium = 中
model-effort-high = 高
model-effort-xhigh = 超高
model-effort-max = 最大

# ---- 状态面板 ----

status-panel-title =  状态 
status-tab-cost = 费用
status-tab-context = 上下文
status-label-duration = 会话时长
status-label-input-tokens = 输入 Tokens
status-label-output-tokens = 输出 Tokens
status-label-cache-create = Cache 创建
status-label-cache-read = Cache 读取
status-label-llm-calls = LLM 调用次数
status-label-estimated-cost = 估算费用
status-label-current-model = 当前模型
status-label-context = 上下文
status-label-used = 已用
status-label-messages = 消息
status-label-tools = 工具
status-empty-data = 暂无请求数据

# ---- Agent 面板 ----

agent-panel-title-none =  Agent 选择 (无) 
agent-panel-title =  Agent 选择 
agent-panel-none-label = 无 Agent（默认）
agent-panel-empty-hint = 在 .claude/agents/ 目录中添加 Agent 定义文件

# ---- Hook 面板 ----

hooks-panel-title-none =  Hook 配置 (无) 
hooks-panel-title =  Hook 配置 
hooks-configured-count = 已配置 { $count } 个 hook
hooks-readonly-hint = 此面板为只读。要添加或修改 hook，请编辑插件的 hooks.json。
hooks-no-hooks =   未配置 hook。
hooks-no-hooks-hint =   Hook 可通过插件 hooks/hooks.json 添加。

# ---- 历史浏览器 ----

thread-browser-title =  恢复会话 ({ $cursor }/{ $total }) 
thread-browser-search-placeholder = 搜索…
thread-browser-empty =   （暂无对话）
thread-browser-no-match =   （无匹配对话）
thread-browser-untitled = （无标题）
thread-browser-time-just-now = 刚刚
thread-browser-time-minutes = { $count } 分钟前
thread-browser-time-hours = { $count } 小时前
thread-browser-time-days = { $count } 天前

# ---- Rewind 弹窗 ----

rewind-title = 回滚
rewind-msg-count = ({ $count }条消息)
rewind-mode-messages = 1. 回到此 prompt
rewind-mode-files = 2. 回到此 prompt + 恢复文件
rewind-mode-confirm = ⚠ 确认: 恢复文件?
rewind-files-to-restore = 将恢复的文件:
rewind-confirm-hint = Enter 确认, Esc 取消
rewind-write-op = Write → 删除+Git restore
rewind-edit-op = Edit → 恢复

# ---- OAuth 弹窗 ----

oauth-title =  OAuth 授权 — { $server } 
oauth-prompt = 按 Ctrl+O 在浏览器中打开链接，完成后粘贴回调 URL：
oauth-callback-label = 回调 URL > 

# ---- Login 面板 ----

login-field-name = 名称
login-field-type = 类型
login-field-base-url = 基础 URL
login-field-api-key = API 密钥
login-field-opus-model = Opus 模型
login-field-sonnet-model = Sonnet 模型
login-field-haiku-model = Haiku 模型

# ---- 配置面板补充 ----

config-lang-display-en = English
config-lang-display-zh = 简体中文
config-lang-display-auto = auto
config-streaming-display-streaming = streaming
config-streaming-display-block = block
config-streaming-display-none = none
config-proactiveness-display-low = low
config-proactiveness-display-medium = medium
config-proactiveness-display-high = high

# ---- 命令输出 ----

command-channel-desc = 管理 MCP 频道连接: open <source> / close / status
command-channel-usage = 用法: /channel open <source> | /channel close | /channel status
command-channel-not-init = Channel 系统未初始化
command-channel-unavailable = 服务器 { $server } 不支持 channel 功能或未连接
command-channel-opened = 频道已开启: { $source }
command-channel-all-closed = 所有频道已关闭
command-channel-closed = 频道已关闭: { $server }
command-channel-no-channels = 没有开启的频道。使用 /channel open <source> 开启
command-channel-list-header = 已开启的频道:
command-channel-list-item =   { $source }
command-bg-usage = 用法: /bg <命令描述>
    例如: /bg 用中文搜索 Rust 2026 roadmap 最新进展
command-loop-usage = 用法: /loop <自然语言时间描述> <提示词>
    例如: /loop 每隔5分钟提醒我喝水
command-plugin-add-failed-detail = 添加 marketplace 失败: { $error }
command-plugin-install-failed = 安装插件失败: { $error }
command-plugin-update-failed = 更新 marketplace 失败: { $error }
command-agent-reset = Agent 已重置（未设置 agent_id）
command-agent-switched = Agent 已切换为: { $name } ({ $id })
command-lang-current-suffix =  (当前)
command-config-save-failed = 配置保存失败: { $error }
command-plugin-help = 用法:
    /plugin                                    — 打开插件面板
    /plugin marketplace add <url>              — 添加市场源
    /plugin install <name>@<marketplace>       — 安装插件
    /plugin marketplace update <name>          — 更新市场缓存

# ---- 消息渲染 ----

render-batch-all-failed = { $count } 个 agent 失败
render-batch-partial = { $done } 个 agent 已完成，{ $failed } 个失败
render-batch-done = { $count } 个 agent 已完成
render-status-failed = 失败
render-status-done = 完成
render-tool-uses = · { $count } 次工具调用
render-user-answered = 用户回答了 Peri 的问题：
render-thought-for = 思考了 { $count } 字符
render-agent-header = Agent

# ---- 消息区 Spinner ----

msg-spinner-tokens = · ↓ { $count } tokens
msg-spinner-brewed =   ✻  处理耗时 { $duration }
msg-tip-prefix =   ⎿  提示: 
msg-todo-available =  (可开始)

# ---- 消息视图占位符 ----

msg-placeholder-image = [图片]
msg-placeholder-document = [文档: { $name }]

# ---- 应用杂项 ----

app-cli-no-input = 无输入 prompt。用法: peri -p "你的问题" 或 echo "问题" | peri -p
app-thread-deleted = 已删除对话: { $title }
app-memory-project = 项目说明
app-memory-user = 用户全局

# ---- 状态栏补充 ----

statusbar-rewind-wait =  Agent 运行中，请等待后再撤销 
statusbar-rewind-pending =  再按 ESC 回滚对话 
statusbar-rewind-action = 回滚对话
statusbar-rewind-other-key = 其他键
statusbar-rewind-move = 移动
statusbar-rewind-switch-file = 切换回退文件
