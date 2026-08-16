# ============================================================
# Peri TUI — English (en) Translation File
# This is the fallback language; keys missing in other
# languages resolve to the values defined here.
# ============================================================

# ---- i18n infrastructure test keys ----
test-hello = Hello, World!
test-greeting = Hello, { $name }!
ui-empty = (none)

# ---- Command Descriptions ----

command-help-description = List all available commands
command-clear-description = Clear message list
command-exit-description = Exit the application
command-compact-description = Compact conversation context (structured summary + re-inject recent files/Skills)
command-model-description = Open model selection panel (Provider + Level + Thinking); with args, switch alias directly (opus/sonnet/haiku)
command-login-description = Manage Provider configuration (create/edit/delete)
command-cost-description = View current session cost and token usage
command-context-description = View context usage and session statistics
command-agents-description = Open Agent selection panel
command-mcp-description = Manage MCP server connections
command-memory-description = Edit user/project-level CLAUDE.md memory files
command-history-description = Open conversation history browser
command-loop-description = Register scheduled loop task (natural language description, e.g. /loop remind me to drink water every 5 minutes)
command-cron-description = View and manage scheduled tasks
command-tasks-description = View agent threads and scheduled tasks
command-plugin-description = Manage plugins (browse, install, uninstall)
command-config-description = Global configuration (autocompact, language, system prompt overrides)
command-hooks-description = View Hook configuration
command-effort-description = View or set reasoning effort (low/medium/high/xhigh/max)
command-rename-description = View or modify current session title
command-lang-description = Switch interface language (e.g. /lang zh-CN)
command-setup-description = Open setup wizard to configure providers
command-agent-description = Set Agent definition, switch different Agent roles

# ---- Command Execution Messages ----

# help command
help-available-commands = Available commands:
help-alias-prefix = (aliases: /{ $aliases })
help-skills-count = Skills ({ $count } available): type # prefix to view
help-skills-empty = Skills: place .md files in .claude/skills/ directory to add
help-shortcuts = Shortcuts: Shift+Tab toggle permission mode | { $model_key } switch model | Shift+Enter newline | Esc quit | Ctrl+C interrupt

# compact command
compact-agent-running = Agent is running, cannot compact

# history command
history-agent-running = Agent is running, cannot open history panel

# model command
config-save-failed = Configuration save failed: { $error }

# effort command
effort-set = Reasoning effort set to { $effort }
effort-current = Current reasoning effort: { $effort }
effort-usage = Usage: /effort low|medium|high|xhigh|max

# loop command
loop-usage = Usage: /loop <natural language time description> <prompt>
loop-example = Example: /loop remind me to drink water every 5 minutes

# rename command
rename-no-session = No active session, cannot rename
rename-current-title = Current title: { $title }
rename-updated = Session title updated to: { $name }
rename-failed = Rename failed: { $error }
rename-untitled = (untitled)

# lang command
lang-switched = Language switched to { $lang }
lang-available = Available languages: { $langs }
lang-unsupported = Unsupported language: { $lang }

# ---- Status Bar ----

statusbar-permission-dont-ask = Don't Ask
statusbar-permission-accept-edit = Accept Edit
statusbar-permission-auto = Auto Mode
statusbar-permission-bypass = Bypass
statusbar-copied =  { $count } chars copied
statusbar-no-agent = None
statusbar-bg-indicator = [BG: { $count }]
statusbar-retrying = Retry { $attempt }/{ $max } ({ $delay }s): { $error }
statusbar-mcp-connecting =  MCP ({ $connected }/{ $total })...
statusbar-mcp-ready =  MCP ready ({ $total } servers)
statusbar-mcp-failed =  MCP failed: { $msg }
statusbar-lsp-diag = diag: { $errors }E/{ $warnings }W

# ---- Status Bar Shortcut Hints ----

statusbar-hint-quit-pending =  Press Ctrl+C again to quit, other keys cancel 
statusbar-hint-popup =  Esc: close | Enter: confirm 
statusbar-hint-menu =  Esc: close | Tab: navigate | Enter: select 
statusbar-hint-main =  /: commands | Shift+Enter: newline | Shift+Tab: mode 

# ---- Welcome Page ----

welcome-title = Peri Agent Framework
welcome-divider = ────── What can I do? ──────
welcome-feature-code = Ask me to code, debug, or refactor
welcome-feature-files = Manage files and run terminal commands
welcome-feature-agents = Delegate tasks to specialized sub-agents
welcome-login-hint-1 = Please type
welcome-login-hint-2 = to configure API Key to get started
welcome-shortcuts = Enter send  |  Shift+Enter newline  |  @ mention files
welcome-shortcut-quit = :Quit
welcome-shortcut-stop = :Stop
welcome-shortcut-newline = :NewLine
welcome-shortcut-mode = :Mode
welcome-shortcut-model = :Model
welcome-skills-available = { $count } skills available

# ---- Tips (18 items) ----

tip-0 = Type / to enter commands, Tab to autocomplete
tip-1 = Ctrl+C interrupts Agent, Shift+Tab toggles permission mode
tip-2 = Ctrl+T switch model (opus / sonnet / haiku), Ctrl+Shift+T switch provider
tip-3 = Shift+Enter for newline in input box
tip-4 = Drag files or images to terminal to auto-attach to message
tip-5 = Long press Ctrl+V to paste clipboard image
tip-6 = Ctrl+U/D scroll message history, Up/Down browse input history
tip-7 = Ctrl+N/P switch Session, Ctrl+W close
tip-8 = Esc closes popup or panel, Enter confirms selection
tip-9 = /compact compresses context to save tokens
tip-10 = /clear clears current conversation
tip-11 = /model switches LLM model
tip-12 = /history browses conversation history
tip-13 = /loop creates scheduled loop tasks
tip-14 = /plugin manages Claude Code plugins
tip-15 = Add custom Skills in .claude/skills/
tip-16 = Define SubAgents in .claude/agents/
tip-17 = For complex tasks, have Agent plan first before executing

# ---- Setup Wizard ----

setup-welcome-title =  ── Peri Setup ── Welcome
setup-choose-provider =  Choose how to configure your provider:
setup-source-custom-api = Custom API
setup-source-migrate = Migrate from Claude Code
setup-source-peri-free = Peri Code Free Service
setup-source-custom-desc = Manually enter provider details
setup-source-migrate-desc = Import config from ~/.claude/
setup-source-peri-free-desc = One-click Peri Code free gateway, no API key needed
setup-key-confirm = :Confirm
setup-key-select = :Select
setup-key-quit = :Quit
setup-configure-title =  ── Peri Setup ── Configure Providers
setup-submit = Submit
setup-key-edit-submit = :Edit/Submit
setup-key-check = :Check
setup-key-back = :Back
setup-edit-title =  ── Setup ── Edit: { $type } ({ $id })
setup-field-type = Type
setup-field-id = ID
setup-field-base-url = Base URL
setup-field-test-connectivity = Test connectivity
setup-hint-base-url-v1 = OpenAI base URL needs /v1 suffix
setup-field-api-key = API Key
setup-field-fable = Fable
setup-field-opus = Opus
setup-field-sonnet = Sonnet
setup-field-haiku = Haiku
setup-model-label = Model
setup-label-key = Key:
setup-provider-anthropic = Anthropic
setup-provider-openai = OpenAI Compatible
setup-confirm = Confirm
setup-test-connectivity = [ Test Connectivity ]
setup-key-switch-type = :Switch type
setup-key-back-list = :Back to list
setup-complete-title =  ── Setup Complete ✓
setup-press-enter = Press
setup-to-start = to start using
setup-no-key = (no key)
setup-no-providers = No providers configured. Add one by selecting "Custom API" or importing from Claude Code.

setup-language-title = ── Peri Setup ── Language
setup-language-prompt = Choose your interface language:
setup-language-press-enter = Press Enter to confirm

# ---- Config Panel ----

config-panel-title =  /config — Configuration
config-field-autocompact = Autocompact
config-field-compact-threshold = Compact Threshold
config-field-language = Language
config-field-persona = Persona
config-field-tone = Tone
config-field-proactiveness = Proactiveness
config-field-cache-warning = Cache Warning
config-field-diff = Show Diff
config-field-1m-context = 1M Context
config-field-active-alias = Active Alias
config-field-permission-mode = Permission Mode
config-value-on = ON
config-value-off = OFF
config-streaming-value-streaming = streaming
config-streaming-value-block = block
config-streaming-value-none = none
config-language-value-en = English
config-language-value-zh = 中文
config-saved = Configuration saved
panel-config-nav-hint =   ↑/↓::navigate  Enter::toggle  ←/→::switch  Esc::close

# Config panel groups
config-group-general = General
config-group-prompt-overrides = Prompt Overrides

# Config field descriptions
config-desc-autocompact = (ON/OFF — auto-compact context when full)
config-desc-threshold = 50-99% — trigger threshold for auto-compact
config-desc-language = en, zh-CN, or leave empty for auto
config-desc-persona = Override system prompt persona (empty = default)
config-desc-tone = Override system prompt tone (empty = default)
config-desc-proactiveness = low / medium / high — agent initiative level
config-desc-cache-warning = (ON/OFF — show low cache hit rate warning in chat)
config-desc-diff = (ON/OFF — show inline diff for Write/Edit tools)
config-field-streaming = Streaming Mode
config-desc-streaming = streaming / block / none — render granularity for LLM output

# Scroll FPS
config-field-scroll-fps = Scroll FPS
config-fps-value-60 = 60fps
config-fps-value-30 = 30fps
config-fps-value-20 = 20fps

# ---- Login Panel ----

login-panel-title-browse =  /login — Provider Management
login-panel-title-edit =  /login — Edit Provider
login-panel-title-new =  /login — New Provider
login-panel-title-confirm-delete =  /login — Confirm Delete
login-no-model = (not set)
login-empty-hint =   (no provider, press Ctrl+N to create)
login-confirm-delete-label =  Confirm delete
login-confirm-delete-question =  ?
login-key-new = :New
login-key-delete = :Delete
login-key-paste = :Paste
login-confirm-delete = :Confirm delete
login-confirm-delete-warning =   This action cannot be undone.
login-confirm = Confirm
login-model-label = Model

# ---- HITL Popup ----

hitl-single-title =  ⚠ Tool Approval (1 item)
hitl-batch-title =  ⚠ Batch Tool Approval
hitl-approved = [Approved]
hitl-rejected = [Rejected]
hitl-summary = Selected: { $approved } approved / { $rejected } rejected

# ---- AskUser Popup ----

ask-user-placeholder = Type something.

# ---- App Messages ----

app-provider-ready = { $name } ({ $model }) ready
app-not-configured = Not configured
app-empty = None
app-no-api-key-warning = Warning: No API Key set (ANTHROPIC_API_KEY or OPENAI_API_KEY)
app-interrupted-resumed = Force interrupted
app-interrupt-done = Interrupted
app-interrupted-background = Force interrupted
app-config-saved = Configuration saved
app-config-save-failed = Configuration save failed: { $error }
app-provider-activated = Provider activated: { $name }
app-provider-created = Provider created and activated: { $name }
app-provider-saved = Provider saved and activated: { $name }
app-provider-deleted = Provider deleted: { $name }
app-provider-name-empty = Save failed: Provider name cannot be empty
app-agent-reset = Agent reset (no agent_id set)
app-agent-switched = Agent switched to: { $name } ({ $id })
app-agent-disconnected = Agent connection lost, please retry sending
app-compact-no-context = No compressible context (history is empty)
app-compact-no-provider = Compact failed: No LLM Provider configured (set ANTHROPIC_API_KEY or OPENAI_API_KEY)
app-compact-compressing = Compressing context
app-compact-done = Context compressed
app-compact-failed = Compact failed: { $error }
app-compact-auto-cleared = Auto cleanup: freed { $count } tool call results
app-compact-limit-reached = Context still exceeds limit after compression. Use /compact to manually compress or /clear to clear history.
app-model-switched = Model switched to: { $alias } ({ $effort } effort)
app-1m-context-enabled = 1M context mode enabled (context window: 1,000,000 tokens)
app-prompt-cache-low = Prompt cache hit rate { $rate }% < 80% (req: { $req })
app-no-mcp-configured = No MCP servers configured (add in .mcp.json or settings.json)
app-no-cron-tasks = No cron tasks
app-cron-deleted = Cron task deleted: { $preview }
app-submit-attachments = { $input } [{ $count } image(s)]
app-no-provider-submit = No API Key configured, type /login to configure Provider
app-bg-task-done = [Background task { $id } completed] Agent: { $agent } | Tool calls: { $tools } | Duration: { $duration }ms
app-bg-task-done-with-result = [Background task { $id } completed] Agent: { $agent } | Tool calls: { $tools } | Duration: { $duration }ms\nResult:\n{ $result }
app-bg-task-failed = [Background task { $id } failed] Agent: { $agent } | { $error }
app-bg-task-failed-with-error = [Background task { $id } failed] Agent: { $agent }\nError:\n{ $error }
app-bg-continuation = Reviewing { $count } background agent result(s)...

# ---- Panel Status Bar Hints ----

# Login panel
hint-login-browse = :Navigate
hint-login-edit = :Edit
hint-login-new = :New
hint-login-delete = :Delete
hint-login-close = :Close
hint-login-field = :Field
hint-login-confirm = :Confirm
hint-login-paste = :Paste
hint-login-toggle = :Toggle
hint-login-back = :Back

# Config panel
hint-config-field = :Field
hint-config-toggle = :Toggle
hint-config-save = :Save & close

# Model panel
hint-model-navigate = :Navigate
hint-model-confirm = :Confirm
hint-model-effort = :Effort
hint-model-close = :Close

# Agent panel
hint-agent-select = :Select
hint-agent-confirm = :Confirm
hint-agent-cancel = :Cancel

# MCP panel
hint-mcp-navigate = :Navigate
hint-mcp-detail = :Detail
hint-mcp-reconnect = :Reconnect
hint-mcp-delete = :Delete
hint-mcp-execute = :Execute
hint-mcp-back = :Back
hint-mcp-close = :Close

# ---- MCP Panel Content ----

mcp-server-count = { $count } servers
mcp-section-project = Project MCPs
mcp-section-project-path = Project MCPs ({ $path })
mcp-section-user = User MCPs
mcp-section-user-path = User MCPs ({ $path })
mcp-section-plugin = Plugin MCPs
mcp-no-servers = No MCP servers configured. Edit .mcp.json or settings.json
mcp-panel-title = Manage MCP servers
# Status
mcp-status-connected = connected
mcp-status-needs-auth = needs authentication
mcp-status-error = error
mcp-status-disabled = disabled
mcp-status-uninitialized = not initialized
mcp-status-offline = offline
# Auth
mcp-auth-authenticated = authenticated
mcp-auth-none = none
# Labels
mcp-label-status = Status:
mcp-label-auth = Auth:
mcp-label-url = URL:
mcp-label-config-location = Config location:
mcp-label-plugin = Plugin
mcp-label-plugin-source = Plugin - { $source }
mcp-label-capabilities = Capabilities:
mcp-label-tools = Tools:
mcp-label-tools-count = { $count } tools
# Capabilities
mcp-capability-tools = tools
mcp-capability-resources = resources
# Actions
mcp-action-hide-tools = Hide tools
mcp-action-view-tools = View tools
mcp-action-reauthenticate = Re-authenticate
mcp-action-clear-auth = Clear authentication
mcp-action-reconnect = Reconnect
mcp-action-disable = Disable
mcp-action-enable = Enable
# OAuth Messages
mcp-oauth-completed = [i] OAuth authorization completed: { $server }
mcp-oauth-failed = [i] OAuth authorization failed: { $server } - { $error }
mcp-oauth-restored = [i] Connected with saved credentials: { $server }
mcp-clear-auth-ok = [i] OAuth credentials cleared: { $server }
mcp-clear-auth-failed = [i] Failed to clear OAuth credentials: { $server }
mcp-action-ok = [i] Action completed: { $server }
mcp-action-failed = [i] Action failed: { $server }

# Plugin panel
hint-plugin-uninstall = :Confirm uninstall
hint-plugin-cancel = :Cancel
hint-plugin-delete = :Confirm delete
hint-plugin-add = :Add
hint-plugin-exit-search = :Exit search
hint-plugin-tab = :Tab
hint-plugin-install = :Install
hint-plugin-remove = :Remove
hint-plugin-navigate = :Navigate
hint-plugin-execute = :Execute
hint-plugin-back = :Back to list
hint-plugin-select = :Select
hint-plugin-search = :Search

# Cron panel
hint-cron-confirm-delete = :Confirm delete
hint-cron-navigate = :Navigate
hint-cron-toggle = :Toggle
hint-cron-delete = :Delete
hint-cron-close = :Close

# Status panel
hint-status-tab = :Switch Tab
hint-status-close = :Close

# History panel
hint-history-confirm-delete = :Confirm delete
hint-history-exit-search = :Exit search
hint-history-close = :Close

# Hooks panel
hint-hooks-navigate = :Navigate
hint-hooks-close = :Close

# Memory panel
hint-memory-select = :Select
hint-memory-edit = :Edit
hint-memory-close = :Close

# ---- Plugin Panel Messages ----

app-plugin-updating = Updating marketplace: { $name }
app-plugin-delete-failed = Delete failed: { $error }
app-plugin-add-failed = Add failed: { $error }
app-plugin-added = Marketplace added: { $name } (fetching content...)

# Background Agent Bar
bg-bar-focus-hint = Press Esc to exit focus

# ---- Model Panel ----

model-panel-title =  Select model 
model-panel-description =   Switch between models. Applies to this session.
model-field-max-token = Max Token
model-field-effort = Effort
model-field-1m-context = 1M Context
model-effort-low = Low
model-effort-medium = Medium
model-effort-high = High
model-effort-xhigh = XHigh
model-effort-max = Max
panel-model-nav-hint =   ↑/↓::switch  Tab::side  →/←::value  Esc::exit/close
panel-model-inline-toggle-hint =   Enter toggle

# ---- Status Panel ----

status-panel-title =  Status 
status-tab-cost = Cost
status-tab-context =  Context
status-label-duration = Session Duration
status-label-input-tokens = Input Tokens
status-label-output-tokens = Output Tokens
status-label-cache-create = Cache Creation
status-label-cache-read = Cache Read
status-label-llm-calls = LLM Calls
status-label-estimated-cost = Est. Cost
status-label-current-model = Current Model
status-label-context = Context
status-label-used = Used
status-label-messages = Messages
status-label-tools = Tools
status-empty-data = No request data
panel-status-nav-hint =   ←/→::switch  Esc::close

status-tab-service =  Service
status-label-provider = Provider:
status-label-model = Model:
status-label-permission = Permission:
status-label-cpu = CPU:
status-label-memory = Memory:
status-label-mcp = MCP:
status-label-cron = Cron:
status-label-cwd = cwd:
status-label-total-vms = Total VMs:
status-label-user-turns =   User turns:
status-label-assistant-turns =   Assistant turns:
status-label-tool-calls =   Tool calls:
status-label-subagent-groups =   SubAgent groups:
status-label-system-notes =   System notes:

# ---- Agent Panel ----

agent-panel-title-none =  Select Agent (None) 
agent-panel-title =  Select Agent 
agent-panel-none-label = No Agent (default)
agent-panel-empty-hint = Add Agent definition files in .claude/agents/
panel-agent-nav-hint =   ↑/↓::navigate  Enter::open  Esc::close

# ---- Agent Session Info Panel ----

agent-panel-title-session =   Current Agent Session
agent-label-provider = Provider
agent-label-model = Model
agent-label-permission-mode = Permission Mode
agent-label-cwd = CWD
agent-label-messages = Messages
agent-label-total-messages = Total Messages
agent-subagents-count =   SubAgents ({ $count })
agent-no-subagents =   No sub-agents spawned in this session
agent-collapsed =  (collapsed)
agent-expanded =  (expanded)
agent-message-count =   { $count } msgs

# ---- Hooks Panel ----

hooks-panel-title-none =  Hooks (none configured) 
hooks-panel-title =  Hooks 
hooks-configured-count = { $count } hooks configured
hooks-readonly-hint = This panel is read-only. To add or modify hooks, edit plugin hooks.json.
hooks-no-hooks =   No hooks configured.
hooks-no-hooks-hint =   Hooks can be added via plugin hooks/hooks.json.
panel-hooks-nav-hint =   ↑/↓::navigate  Enter::open  Esc::close
hook-event-before-tool = Before tool execution
hook-event-after-tool = After tool execution
hook-event-after-tool-fail = After tool execution fails
hook-event-before-auto-mode = Before auto mode classifier decides
hook-event-user-submit = When user submits a prompt
hook-event-session-start = When a new session starts
hook-event-session-end = When a session ends
hook-event-agent-stop = When agent stops
hook-event-agent-stop-fail = When agent stops with failure
hook-event-parallel-tools-done = When all parallel tools complete
hook-event-subagent-start = When a subagent starts
hook-event-subagent-stop = When a subagent stops
hook-event-before-compact = Before context compaction
hook-event-after-compact = After context compaction
hook-event-needs-input = When agent needs user input

# ---- Theme Panel ----

theme-desc = Switch color theme
theme-title = Theme
theme-preview = Preview
theme-list = Theme List
theme-confirm = Confirm
theme-cancel = Cancel
theme-current = Current
theme-source-builtin = Builtin
theme-source-file = File
theme-switched = Theme switched
theme-navigate = Navigate

# ---- Thread Browser ----

thread-browser-title =  Resume Session ({ $cursor }/{ $total }) 
thread-browser-search-placeholder = Search…
thread-browser-empty =   (No conversations yet)
thread-browser-no-match =   (No matching conversations)
thread-browser-untitled = (untitled)
thread-browser-time-just-now = just now
thread-browser-time-minutes = { $count } minute{ $suffix } ago
thread-browser-time-hours = { $count } hour{ $suffix } ago
thread-browser-time-days = { $count } day{ $suffix } ago
panel-threads-header-hint =   Enter::open · Esc::close
panel-threads-nav-hint =   ↑/↓::navigate  Enter::open  d::delete  Esc::close
panel-threads-confirm-hint =   Enter::confirm  Esc::cancel

# ---- Rewind Popup ----

rewind-title = Rewind
rewind-msg-count = ({ $count }msg)
rewind-mode-messages = 1. Back to this prompt
rewind-mode-files = 2. Back to this prompt + restore files
rewind-mode-confirm = ⚠ Confirm: restore files?
rewind-files-to-restore = Files to restore:
rewind-confirm-hint = Enter to confirm, Esc to cancel
rewind-write-op = Write → Delete + Git restore
rewind-edit-op = Edit → Restore
# ---- Rewind v2（popup & consumer copy）----
rewind-executing = Rewinding…
rewind-budget-title = Rewind will revert { $count } file change(s):
rewind-budget-more = ... and { $count } more
rewind-budget-confirm-hint = Enter to confirm · Esc back to candidates
rewind-query-failed = Query failed: { $error }
rewind-loading = Loading rewind candidates…
rewind-empty = Nothing to rewind.
rewind-empty-hint = Complete a turn, then double-press Esc to rewind.
rewind-title-count = Rewind to ({ $count })
rewind-enter-hint = Enter to rewind · Esc to close
rewind-error-no-client = ACP client not initialized, cannot query candidates
rewind-error-no-session = No active session, cannot query candidates
rewind-error-query-failed = Candidates query failed: { $error }
rewind-error-budget-missing = rewind-preview response missing file_changes array
rewind-error-path-missing = budget item missing path
rewind-execute-failed = Rewind failed: { $error }

# ---- OAuth Popup ----

oauth-title =  OAuth Authorization — { $server } 
oauth-prompt = Choose "Open browser" to authorize, then paste the authorization code into the input:
oauth-callback-label = Authorization code > 
oauth-btn-open = Open browser
oauth-btn-copy = Copy link
oauth-btn-cancel = Cancel
oauth-hint-btn-focus =   ←→: select button  |  Enter: activate  |  Tab: type code  |  Esc: cancel
oauth-hint-input-focus =   Paste authorization code, Enter to submit  |  Tab: buttons  |  Esc: cancel
oauth-copied-hint =   ✓ Link copied (open it in your browser)
oauth-opened-hint =   Browser opened — if not, copy the link and open manually

# ---- Login Panel ----

login-field-name = Name
login-field-type = Type
login-field-base-url = Base URL
login-field-api-key = API Key
login-field-fable-model = Fable Model
login-field-opus-model = Opus Model
login-field-sonnet-model = Sonnet Model
login-field-haiku-model = Haiku Model

# ---- Config Panel additional ----

config-lang-display-en = English
config-lang-display-zh = 简体中文
config-lang-display-auto = auto
config-streaming-display-streaming = streaming
config-streaming-display-block = block
config-streaming-display-none = none
config-proactiveness-display-low = low
config-proactiveness-display-medium = medium
config-proactiveness-display-high = high

# ---- Command Outputs ----

command-channel-desc = Manage MCP channel connections: open <source> / close / status
command-channel-usage = Usage: /channel open <source> | /channel close | /channel status
command-channel-not-init = Channel system not initialized
command-channel-unavailable = Server { $server } does not support channel or is not connected
command-channel-opened = Channel opened: { $source }
command-channel-all-closed = All channels closed
command-channel-closed = Channel closed: { $server }
command-channel-no-channels = No open channels. Use /channel open <source> to open
command-channel-list-header = Open channels:
command-channel-list-item =   { $source }
command-bg-usage = Usage: /bg <command description>
    Example: /bg Search for Rust 2026 roadmap latest progress in Chinese
command-loop-usage = Usage: /loop <natural language time> <prompt>
    Example: /loop remind me to drink water every 5 minutes
command-plugin-add-failed-detail = Add marketplace failed: { $error }
command-plugin-install-failed = Install plugin failed: { $error }
command-plugin-update-failed = Update marketplace failed: { $error }
command-agent-reset = Agent reset (no agent_id set)
command-agent-switched = Agent switched to: { $name } ({ $id })
command-lang-current-suffix =  (current)
command-config-save-failed = Config save failed: { $error }
command-plugin-help = Usage:
    /plugin                                    — Open plugin panel
    /plugin marketplace add <url>              — Add marketplace source
    /plugin install <name>@<marketplace>       — Install plugin
    /plugin marketplace update <name>          — Update marketplace cache

# ---- Message Rendering ----

render-batch-all-failed = { $count } agents failed
render-batch-partial = { $done } agents finished, { $failed } failed
render-batch-done = { $count } agents finished
render-status-failed = Failed
render-status-done = Done
render-tool-uses = · { $count } tool uses
render-user-answered = User answered Peri's questions:
render-thought-for = Thought for { $count } chars
render-more-lines = … +{ $count } lines
render-todo-summary = { $done }/{ $total } tasks
render-todo-summary-active = { $done }/{ $total } tasks · { $active }
render-agent-header = Agent

# ---- Message Area Spinner ----

msg-spinner-tokens = · ↓ { $count } tokens
msg-spinner-brewed =   ✻  Brewed for { $duration }
msg-keepgoing = Keep Going
msg-copy-md = Copy
msg-tip-prefix =   ⎿  Tip: 
msg-todo-available =  (available)

# ---- @image lines (image-p0-p1-spec §4 T4) ----
# 用户气泡 `@image <path>` 行的 meta 行文案；$name 为文件名（hover 时为绝对路径），
# $size 为人类可读大小（user-image-size-*）或缺失文案（user-image-missing）。
user-image-meta = [Image: { $name } · { $size }]
user-image-missing = missing
user-image-size-bytes = { $count } B
user-image-size-kb = { $count } KB
user-image-size-mb = { $count } MB
user-image-open-failed = Failed to open image

# ---- image preview overlay (image-p0-p1-spec §7 T7) ----
# 上下文 overlay 预览：meta 行（$w/$h 为像素尺寸，JPEG/GIF/WebP 未解析时为 0）、
# 解码中行、手工路径降级提示、校验/解码失败固定文案（不显示原因细节）。
image-preview-meta = [Image: { $name } · { $w }×{ $h } · { $size } · { $mime }]
image-preview-loading = [Image: { $name }]
image-preview-degraded = Only images inside the managed folder (~/.peri/images) can be previewed
image-preview-error = This image cannot be previewed
image-preview-no-protocol = This terminal doesn't support image display (Kitty graphics); showing text info

# ---- Message Entry Status Fallbacks (spec §4.1: symbols' text fallbacks) ----
# Unicode 能力不足时，状态由符号退化为明确文本；同时 serve as aria/语义后备。
msg-status-running = Running
msg-status-done = Done
msg-status-failed = Failed
msg-status-needs-approval = Needs approval
msg-status-collapsed = collapsed
msg-status-expanded = expanded
msg-status-queued = Queued
msg-user-prompt = You
msg-assistant-prompt = Perihelion
msg-status-loading = Loading
msg-new-output = New output
render-group-failed-count = { $count } failed

# ---- Interaction block (spec §6.8, Slice 4) ----
# inline transcript block 与 AskUser 面板 / HITL 弹窗双轨（D5）——
# result 文案为纯文本（无符号），渲染层负责状态符号与颜色。

render-interaction-title-permission = Approval required
render-interaction-title-ask-user = Ask User
render-interaction-question-permission = { $verb } wants to run: { $summary }
render-interaction-tool-unknown = unknown tool
render-interaction-allow-once = Allow once
render-interaction-deny = Deny
render-interaction-result-allowed-once = Allowed once
render-interaction-result-denied = Denied
render-interaction-result-answered = Answered
render-interaction-result-rejected = Rejected

# ---- Composer (spec §10) ----

composer-attachments = @ { $count } files
composer-context-usage = { $pct }% ctx

# ---- Message View Placeholders ----

msg-placeholder-image = [Image]
msg-placeholder-document = [Document: { $name }]

# ---- App Misc ----

app-cli-no-input = No input prompt. Usage: peri -p "your question" or echo "question" | peri -p
app-thread-deleted = Conversation deleted: { $title }
app-memory-project = Project Description
app-memory-user = User Global

# ---- Status Bar additional ----

statusbar-rewind-wait =  Agent is running, wait before rewind 
statusbar-rewind-pending =  Press ESC again to rewind 
statusbar-rewind-action = Rewind
statusbar-rewind-other-key = Other keys
statusbar-rewind-move = Move
statusbar-rewind-switch-file = Switch restore file

# ---- Common (P0) ----
common-loading = Loading
common-esc-close =   Esc: close
common-nav-enter-close =   ↑/↓::navigate  Enter::open  Esc::close
common-empty =   (empty)

# ---- Setup Wizard (P0) ----
setup-no-provider = No provider configured · Agent features unavailable
setup-config-hint-title = Configure via any of the following:
setup-close-hint = Enter::close · Esc::close
setup-step-1 =   1. Open the Login panel to configure API Key
setup-step-2 =   2. Or open the Settings panel to adjust provider config
setup-step-3 =   3. Or manually edit 
setup-skip-hint = Enter::skip · Esc::close
setup-wizard-title =  Setup Wizard 
setup-welcome = Welcome to Peri TUI

# ---- Notifications (P0) ----
paste-truncated = Paste truncated to { $max } characters
submit-blocked = Request in progress, try again later
export-success = Exported messages to: { $path }
export-fail = Failed to export messages: { $error }
cancel-request-sent = Cancel request sent
bg-task-notify-completed = [✓] { $name } completed ({ $duration }s)
bg-task-notify-failed = [✗] { $name } failed ({ $duration }s)

# ---- Thread Load (P0) ----
thread-switch-confirm-title = Switch Thread Confirmation
thread-switch-bg-tasks-message = Current thread has { $count } background tasks still running
thread-switch-task-counts =   { $shell } shell  { $agent } agent  { $workflow } workflow
thread-switch-bg-note = These tasks continue running after switch, but will not be visible in the current view.

# ---- System Reminders (P0) ----
reminder-cron-task = Cron Task
reminder-bg-task = Background Task
reminder-fork-mode = Fork Mode
reminder-context-compaction = Context Compaction
reminder-system-prompt = System Prompt
reminder-trust-boundary = Trust Boundary
reminder-tool-reminder = Tool Reminder
reminder-subagent-result = SubAgent Result
reminder-system-reminder = System Reminder
channel-wechat = WeChat
channel-feishu = Feishu
channel-dingtalk = DingTalk

# ---- Common (P1) ----
common-no-matches =   (no matches)
common-na = —
common-on = ON
common-off = OFF

# ---- Panel Titles ----
panel-title-model = Model
panel-title-login = Login
panel-title-agent = Agent
panel-title-hooks = Hooks
panel-title-config = Config
panel-title-threads = Threads
panel-title-mcp = MCP
panel-title-plugin = Plugin
panel-title-cron = Cron
panel-title-status = Status
panel-title-memory = Memory
panel-title-tasks = Tasks
panel-title-betas = Betas
panel-title-workflow = Workflow
panel-title-ask-user = Ask User
panel-title-theme = Theme
panel-title-subagent-detail = SubAgent Detail

# ---- Panel Descriptions ----
panel-desc-model = Model alias selection
panel-desc-login = Provider credentials
panel-desc-agent = Subagent definitions
panel-desc-hooks = Hook events
panel-desc-config = PeriConfig editor
panel-desc-threads = Thread history browser
panel-desc-mcp = MCP server pool
panel-desc-plugin = Installed plugins
panel-desc-cron = Scheduled tasks
panel-desc-status = Service snapshot
panel-desc-memory = Persisted memory
panel-desc-tasks = Background tasks
panel-desc-betas = Feature flags
panel-desc-workflow = Workflow runs
panel-desc-ask-user = Agent user questions (auto-open)
panel-desc-theme = Color theme selection
panel-desc-subagent-detail = Subagent nested transcript detail
subagent-detail-not-found = Subagent not found — the session may have been reset.

# ---- Betas Panel ----
panel-betas-readonly-hint =   (read-only — feature flags are configured at build time)
panel-betas-empty =   No active beta features
panel-betas-nav-hint =   ↑/↓::navigate  Enter::open  Esc::close

# ---- Cron Panel ----
panel-cron-stats =   { $configured } configured, { $enabled } enabled
panel-cron-confirm-hint =   Enter::confirm  Esc::close
panel-cron-nav-hint =   ↑/↓::navigate  Enter::toggle  Esc::close
panel-cron-empty =   No cron tasks configured
panel-cron-empty-hint =   Ask the agent to set up recurring tasks
panel-cron-next-fire =      next: { $time }
panel-cron-status-on = ON
panel-cron-status-off = OFF
panel-cron-status-format = [{ $status }]

# ---- MCP Panel ----
panel-mcp-phase-pending = pending
panel-mcp-phase-initializing = initializing
panel-mcp-phase-ready = ready
panel-mcp-phase-failed = failed
panel-mcp-pool-label =   MCP Pool: 
panel-mcp-connected =    { $connected }/{ $total } connected
panel-mcp-empty =   No MCP servers configured
panel-mcp-empty-hint =   Add servers via ~/.claude/settings.json (mcpServers)
panel-mcp-server-detail =      transport: { $transport }  tools: { $count }
panel-mcp-needs-auth =  [needs auth]
panel-mcp-oauth-hint =   Enter: open auth detail  |  Esc: close
# MCP panel detail view (OAuth authorize entry)
panel-mcp-detail-url = URL:
panel-mcp-detail-btn-auth = Authorize
panel-mcp-detail-btn-back = Back
panel-mcp-detail-hint =   Enter: authorize  |  Esc: back to list  |  ←→: select button
panel-mcp-icon-connected = ✔
panel-mcp-icon-error = ✗
panel-mcp-icon-unknown = ◯

# ---- Memory Panel ----
panel-memory-stats =   { $count } memory files in ~/.claude/memory
panel-memory-nav-hint =   Enter) Edit in $EDITOR  Esc) Close
panel-memory-empty =   No memory files found
panel-memory-empty-hint =   Create ~/.claude/memory/<name>.md to persist cross-session notes
panel-memory-unit-b = B
panel-memory-unit-kb = KB
panel-memory-unit-mb = MB
panel-memory-unit-gb = GB
panel-memory-time-just-now = just now
panel-memory-time-min-ago = { $n }m ago
panel-memory-time-hour-ago = { $n }h ago
panel-memory-time-day-ago = { $n }d ago

# ---- Plugin Panel ----
panel-plugin-stats =   { $count } plugins loaded
panel-plugin-readonly-hint =   (read-only — toggle via ~/.claude/plugins/config.json)
panel-plugin-empty =   No plugins installed
panel-plugin-empty-hint =   Install via: agm install <name>
panel-plugin-version-unknown = ?

# ---- Plugin Panel Tabs ----
panel-plugin-tab-installed = Installed
panel-plugin-tab-discover = Discover
panel-plugin-tab-marketplaces = Marketplaces
panel-plugin-tab-errors = Errors

# ---- Plugin Panel Discover ----
panel-plugin-discover-coming = Discover — coming in Phase 2
panel-plugin-discover-hint = Search and install plugins from marketplaces
panel-plugin-discover-install-user = Install (User scope)
panel-plugin-discover-install-project = Install (Project scope)
panel-plugin-discover-field-version = Version
panel-plugin-discover-field-marketplace = Marketplace
panel-plugin-discover-field-author = Author
panel-plugin-discover-field-description = Description

# ---- Plugin Panel Marketplaces ----
panel-plugin-marketplaces-coming = Marketplaces — coming in Phase 2
panel-plugin-marketplaces-hint = Manage plugin marketplaces

# ---- Plugin Panel Errors ----
panel-plugin-errors-coming = Errors — coming in Phase 2
panel-plugin-errors-hint = View plugin load errors

# ---- Plugin Panel Detail ----
panel-plugin-detail-title = Detail: { $name }
panel-plugin-detail-marketplace = marketplace
panel-plugin-detail-author = author
panel-plugin-detail-path = path
panel-plugin-detail-scope = scope
panel-plugin-detail-error = load error

# ---- Plugin Panel Actions ----
panel-plugin-action-disable = Disable plugin
panel-plugin-action-enable = Enable plugin
panel-plugin-action-uninstall = Uninstall
panel-plugin-action-update = Update
panel-plugin-action-back = Back to plugin list
panel-plugin-detail-actions = Actions

# ---- Plugin Panel Fields ----
panel-plugin-field-skills = Skills
panel-plugin-field-commands = Commands
panel-plugin-field-agents = Agents
panel-plugin-field-mcp = MCP

# ---- Plugin Panel Discover ----
panel-plugin-discover-search = Search plugins...
panel-plugin-discover-empty = No results found

# ---- Plugin Panel Marketplaces ----
panel-plugin-marketplaces-add = Add marketplace...
panel-plugin-marketplace-add-label = Add:
panel-plugin-marketplaces-delete = Delete
panel-plugin-marketplaces-empty = No marketplaces configured

# ---- Plugin Panel Errors ----
panel-plugin-errors-title = Load Errors
panel-plugin-errors-empty = No errors

# ---- Plugin Panel Status ----
panel-plugin-status-enabled = enabled
panel-plugin-status-disabled = disabled

# ---- Plugin Panel Confirm ----
panel-plugin-confirm-uninstall = ⚠ Confirm uninstall? Enter to confirm, Esc to cancel
panel-plugin-confirm-delete-mp = ⚠ Confirm delete marketplace? Enter to confirm, Esc to cancel
panel-plugin-confirm-hint = Enter: confirm  Esc: cancel

# ---- Plugin Panel Marketplace ----
panel-plugin-marketplaces-online = online
panel-plugin-marketplaces-offline = offline
panel-plugin-marketplace-refreshing = Refreshing...
panel-plugin-marketplace-hint-keys = Enter: detail/add  |  ↑/↓: navigate  |  Esc: close
panel-plugin-marketplace-add-url-hint = Enter URL (github.com/org/repo, /path/to/dir, etc.)
panel-plugin-marketplace-add-input-footer = Enter: save  Esc: cancel
panel-plugin-marketplace-action-refresh = Refresh
panel-plugin-marketplace-action-delete = Delete
panel-plugin-marketplace-detail-hint = ↑/↓: select  |  Enter: execute  |  Esc: back

# ---- Plugin Panel Search ----
panel-plugin-discover-input = Type to search...

# ---- Plugin Panel Search ----
panel-plugin-search-loading = Searching...
panel-plugin-search-no-results = No results found
panel-plugin-search-error = Search failed: { $error }
panel-plugin-operation-complete = operation complete
panel-plugin-operation-failed = operation failed
panel-plugin-discover-press-enter = Press Enter to search
panel-plugin-action-install = Install
panel-plugin-list-count = { $count } plugins found
panel-plugin-discover-hint-keys = Enter: details  |  type: filter  |  ←/→/Tab: switch view

# ---- Plugin Panel Navigation ----
common-nav-tab-close = ←/→/Tab switch view · ↑/↓ navigate · Enter close · Esc close

# ---- Tasks Panel ----
panel-tasks-total-label =   Total: 
panel-tasks-breakdown =    ({ $bg } bg, { $cron } cron, { $subagent } subagent)
panel-tasks-section-bg =   ▼ Background Tasks ({ $count })
panel-tasks-kind-sh = [sh]
panel-tasks-kind-ag = [ag]
panel-tasks-kind-wf = [wf]
panel-tasks-kind-unknown = [?]
panel-tasks-pid =  pid:{ $pid }
panel-tasks-section-cron =   ▼ Cron Jobs ({ $count })
panel-tasks-section-subagent =   ▼ SubAgents ({ $count })
panel-tasks-collapsed =  (collapsed)
panel-tasks-live =  (live)
panel-tasks-msgs =   { $count } msgs
panel-tasks-empty =   No active tasks
panel-tasks-empty-hint-1 =   Cron jobs are scheduled via /loop command;
panel-tasks-empty-hint-2 =   SubAgents are spawned by Task / SubAgent tools.
panel-tasks-nav-hint =   ↑/↓::navigate  Enter::open  Esc::close

# ---- Theme Panel ----
panel-theme-active-mark =  *
panel-theme-nav-hint =   ↑/↓::navigate  Enter::switch  Esc::close
panel-theme-empty =   (no themes found)
panel-theme-preview = Preview
panel-theme-tab-dark = Dark
panel-theme-tab-light = Light
panel-theme-tab-hint =   Tab::switch-category
panel-theme-daily-on = ON
panel-theme-daily-off = OFF
panel-theme-download-label = download from github
panel-theme-footer-hint =   Ctrl+T::daily({ $status })  Ctrl+D::{ $download }

# ---- Workflow Panel kanban ----
workflow-loading-runs = Loading workflow runs
workflow-no-runs = No workflow runs in current session
workflow-footer-shortcuts = Tab::next-run · Shift+Tab::prev-run · ←/→::pane · ↑/↓::navigate · Enter::inspect · Esc::close
workflow-phases-header = Phases ({ $count } agents)
workflow-model-header = Model

# ---- AskUser Panel ----
panel-ask-user-empty =   No pending questions.
panel-ask-user-malformed =   Agent asked 0 questions (malformed request).
panel-ask-user-answered-mark =  ✓ 
panel-ask-user-no-options =   (no options provided)
panel-ask-user-hint-tab-multi-answered =   Tab::next-question · ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
panel-ask-user-hint-tab-multi-unanswered =   Tab::next-question · ↑/↓::navigate · Space::select · Enter::next · Esc::cancel
panel-ask-user-hint-single-answered =   ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
panel-ask-user-hint-single-unanswered =   ↑/↓::navigate · Space::select · Esc::cancel
panel-ask-user-hint-tab-multi-select-answered =   Tab::next-question · ↑/↓::navigate · Space::toggle · Enter::submit · Esc::cancel
panel-ask-user-hint-tab-multi-select-unanswered =   Tab::next-question · ↑/↓::navigate · Space::toggle · Enter::next · Esc::cancel
panel-ask-user-hint-single-multi-select-answered =   ↑/↓::navigate · Space::toggle · Enter::submit · Esc::cancel
panel-ask-user-hint-single-multi-select-unanswered =   ↑/↓::navigate · Space::toggle · Esc::cancel
panel-ask-user-hint-typing =   Typing · Ctrl+W::delete-word · Backspace::delete · Enter::confirm · Esc::cancel

# ---- Others ----
bg-task-overflow = … { $count } more
bg-task-tools-running = { $name } · { $count } tools
bg-task-tools-done = · { $count } tools
tool-name-shell = Shell
tool-name-folder = Folder
mention-popup-title =  @{ $name } 
slash-completion-title =  /{ $name } 

# ---- HITL Popup (P2) ----
popup-hitl-empty =   No pending approval request.
popup-hitl-tool-label =   Tool: { $name }
popup-hitl-non-serializable = <non-serializable>
popup-hitl-truncated-info =     ... ({ $chars } chars total)
popup-hitl-batch-header =   Batch ({ $more } more):
popup-hitl-batch-item =     - { $name } ({ $input })
popup-hitl-batch-more =     ... and { $count } more
popup-hitl-action-hint =   Enter: approve  |  Esc: reject
popup-hitl-title =  Approval Required 

# ---- AskUser Popup (P2) ----
popup-ask-user-empty =   No pending questions.
popup-ask-user-malformed =   Agent asked 0 questions (malformed request).
popup-ask-user-answered-mark =  ✓ 
popup-ask-user-no-options =   (no options provided)
popup-ask-user-hint-multi-submit =   Tab::next-question · ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
popup-ask-user-hint-multi-next =   Tab::next-question · ↑/↓::navigate · Space::select · Enter::next · Esc::cancel
popup-ask-user-hint-single-submit =   ↑/↓::navigate · Space::select · Enter::submit · Esc::cancel
popup-ask-user-hint-single-unsubmitted =   ↑/↓::navigate · Space::select · Esc::cancel
popup-ask-user-title =  Ask User 

# ---- Confirm Popup (P2) ----
popup-confirm-empty =   No pending confirmation.
popup-confirm-action-hint =   Enter: confirm  Esc: cancel
popup-confirm-title =  Confirm 
popup-confirm-reject-title = Reject Answer
popup-confirm-reject-message = Reject answering? The Agent will receive a rejection signal and end the tool call.

# ---- Download Progress Popup ----

popup-download-title-active = Downloading themes ({ $done }/{ $total })
popup-download-title-done = Download completed ({ $total } files, { $success } success, { $failed } failed)
popup-download-footer-active = Downloading... please wait
popup-download-footer-done = Esc::close
popup-download-empty = (no files to download)
popup-download-finished-notify = Theme download complete: { $success }/{ $total } success, { $failed } failed

# ---- System Notes (app message stream) ----
app-note-budget-warning = Context usage { NUMBER($pct, maximumFractionDigits: 0) }% ({ $used }/{ $limit })
app-note-compact-completed = { $type } completed{ $detail }
app-note-compact-completed-summary = { $type } completed{ $detail } — { $summary }
app-note-compact-error = Context compaction failed: { $message }
app-note-rewind-error = Rewind failed: { $message }
app-note-compact-type-full = Full compaction
app-note-compact-type-micro = Micro compaction
app-note-compact-type-smart = Smart compaction
app-note-agent-failed = Agent execution failed: { $message }
app-note-cache-hit-low = Prompt cache hit rate {$pct}% < 80% (req: { $req_id })

# ---- Semantic tool cards ----
tool-todo-failed = Update failed
