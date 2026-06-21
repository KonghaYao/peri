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

# ---- Status Bar Shortcut Hints (main view) ----

key-command = command
key-switch-session = :Switch Session
key-close = :Close
key-scroll = :Scroll
key-cancel = :Cancel
key-newline = :NewLine
key-open-browser = :Open browser
key-submit = :Submit
key-switch = :Switch
key-switch-tab = :Switch Tab
key-move = :Move
key-select = :Select
key-confirm = :Confirm
key-delete = :Delete
key-reconnect = :Reconnect
key-detail = :Detail
key-execute = :Execute
key-back = :Back
key-install = :Install
key-tab = :Tab
key-effort = :Effort
key-switch-model = :Switch Model

# ---- Welcome Page ----

welcome-title = Peri Agent Framework
welcome-divider = ────── What can I do? ──────
welcome-feature-code = Ask me to code, debug, or refactor
welcome-feature-files = Manage files and run terminal commands
welcome-feature-agents = Delegate tasks to specialized sub-agents
welcome-login-hint-1 = Please type
welcome-login-hint-2 = to configure API Key to get started
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
setup-source-custom-desc = Manually enter provider details
setup-source-migrate-desc = Import config from ~/.claude/
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
setup-hint-base-url-v1 = OpenAI base URL needs /v1 suffix
setup-field-api-key = API Key
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
config-field-diff = Inline Diff
config-value-on = ON
config-value-off = OFF
config-saved = Configuration saved

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

# ---- Login Panel ----

login-panel-title-browse =  /login — Provider Management
login-panel-title-edit =  /login — Edit Provider
login-panel-title-new =  /login — New Provider
login-panel-title-confirm-delete =  /login — Confirm Delete
login-no-model = (not set)
login-empty-hint =   (no provider, press Ctrl+N to create)
login-confirm-delete-label =  Confirm delete
login-confirm-delete-question =  ?
login-key-activate = :Activate
login-key-new = :New
login-key-delete = :Delete
login-key-paste = :Paste
login-confirm-delete = :Confirm delete

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
hint-login-activate = :Activate
hint-login-edit = :Edit
hint-login-new = :New
hint-login-delete = :Delete
hint-login-close = :Close
hint-login-field = :Field
hint-login-save = :Save
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

# ---- Status Panel ----

status-panel-title =  Status 
status-tab-cost = Cost
status-tab-context = Context
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

# ---- Agent Panel ----

agent-panel-title-none =  Select Agent (None) 
agent-panel-title =  Select Agent 
agent-panel-none-label = No Agent (default)
agent-panel-empty-hint = Add Agent definition files in .claude/agents/

# ---- Hooks Panel ----

hooks-panel-title-none =  Hooks (none configured) 
hooks-panel-title =  Hooks 
hooks-configured-count = { $count } hooks configured
hooks-readonly-hint = This panel is read-only. To add or modify hooks, edit plugin hooks.json.
hooks-no-hooks =   No hooks configured.
hooks-no-hooks-hint =   Hooks can be added via plugin hooks/hooks.json.

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

# ---- OAuth Popup ----

oauth-title =  OAuth Authorization — { $server } 
oauth-prompt = Press Ctrl+O to open in browser, then paste callback URL:
oauth-callback-label = Callback URL > 

# ---- Login Panel ----

login-field-name = Name
login-field-type = Type
login-field-base-url = Base URL
login-field-api-key = API Key
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
render-agent-header = Agent

# ---- Message Area Spinner ----

msg-spinner-tokens = · ↓ { $count } tokens
msg-spinner-brewed =   ✻  Brewed for { $duration }
msg-tip-prefix =   ⎿  Tip: 
msg-todo-available =  (available)

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
