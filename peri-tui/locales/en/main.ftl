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
command-cron-list-description = View and manage scheduled tasks
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
config-handle-missing = Configuration unavailable: { $handle } is not initialized
config-draft-conflict = Configuration changed; reload before saving
config-runtime-sync-failed = Configuration saved, but runtime sync failed: { $error }
model-panel-active-provider-unavailable = Selected profile has no usable provider or model. Some Responses models require a context window before switching.
config-reloaded = Configuration reloaded
config-reload-failed = Configuration reload failed: { $error }

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
statusbar-initializing = Initializing…
statusbar-preparing = Preparing session…
composer-submitting = Submitting…
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
setup-source-chatgpt = Sign in with a ChatGPT subscription
setup-source-chatgpt-desc = Use Peri OAuth; never import Codex credentials
setup-chatgpt-title =  ── Peri Setup ── ChatGPT subscription
setup-chatgpt-intro = Connect Peri with your ChatGPT subscription
setup-chatgpt-login-step = 01  Sign in and save the Provider in Login
setup-chatgpt-login-detail =      Authorize with Peri OAuth; this page collects no credentials.
setup-chatgpt-model-step = 02  Configure the active tier in Model
setup-chatgpt-model-detail =      On first setup, set the model; known subscription models use a 1M default window.
setup-chatgpt-privacy =      No Codex credential import or configuration changes on this page.
setup-chatgpt-continue = Continue to Login
setup-chatgpt-cancel = Back to source selection
setup-chatgpt-complete-model = Set the active tier's Provider and Model in /model; set a window if required, then press Esc.
setup-chatgpt-login-hint = Use /login to sign in and save the Provider. Next: configure the active tier in /model.
setup-source-preset = Provider presets
setup-source-custom-desc = Manually enter provider details
setup-source-migrate-desc = Import config from ~/.claude/
setup-source-preset-desc = Pre-fill endpoint and model tiers for a known provider
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
setup-provider-openai-responses = OpenAI Responses
model-panel-edit-window-hint = Enter to edit the exact context window for this profile.
model-panel-context-window = Context window
model-panel-window-edit-controls = Enter save · Ctrl+U clear · Esc cancel
model-panel-window-invalid = Enter a positive whole number of tokens.
model-panel-provider-manage-hint = Ctrl+L manage providers · auth: /login
setup-confirm = Confirm
setup-test-connectivity = [ Test Connectivity ]
setup-key-switch-type = :Switch type
setup-key-back-list = :Back to list
setup-complete-title =  ── Review Setup
setup-press-enter = Press
setup-to-start = to save and activate
setup-save-failed = Could not save setup. Check the configuration file and permissions; restart if it changed externally, then retry.
setup-no-key = (no key)
setup-no-providers = No providers configured. Add one by selecting "Custom API" or importing from Claude Code.

setup-language-title = ── Peri Setup ── Language
setup-language-prompt = Choose your interface language:
setup-language-press-enter = Press Enter to confirm

setup-preset-title =  ── Peri Setup ── Provider presets
setup-preset-prompt =   Choose a provider preset:

# ---- Config Panel ----

config-panel-title =  /config — Configuration
config-beta-section = Beta · takes effect in new sessions
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
config-field-permission-mode = Active Session Permissions
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
config-desc-cache-warning = (ON/OFF — show low final cache coverage warning in chat)
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
login-panel-title-edit = /login — Edit Provider
login-panel-title-new = /login — New Provider
login-panel-title-confirm-delete = /login — Confirm Delete
login-no-model = (not set)
login-browse-plan-unchecked = ChatGPT plan · open the editor to check status
login-empty-hint =   (no provider, press Ctrl+N to create)
login-confirm-delete-label =  Confirm delete
login-confirm-delete-question =  ?
login-key-new = :New
login-key-delete = :Delete
login-key-paste = :Paste
login-confirm-delete = :Confirm delete
login-confirm-delete-warning =   This action cannot be undone.
login-confirm = Confirm
login-model-defaults-source-missing = No saved default models to reuse; fill the fields above or configure them in the Model panel
login-model-label = Default models
login-field-auth-mode = Authentication
login-field-store = Credentials store
login-field-account = Account reference
login-field-subject = Identity
login-field-client-id = Client ID
login-section-basic = Basic
login-section-auth = Authentication
login-section-advanced = Advanced
login-status-label = Sign-in status
login-auth-signed-in-no-email = Signed in
login-error-credentials-incomplete = Incomplete credential reference; sign in to ChatGPT or use a verified account
login-auth-api-key = API key
login-auth-chatgpt-plan = ChatGPT plan
login-auth-actions = Actions
login-auth-endpoint-locked = official endpoint, locked
login-auth-plan-endpoint-warning = Saving will replace the custom Base URL with the official Responses endpoint.
login-auth-unchecked = ChatGPT plan · status not checked
login-auth-checking = Checking ChatGPT status…
login-auth-starting = Starting ChatGPT sign-in…
login-auth-waiting = Waiting for the browser callback…
login-auth-cancelling = Stopping the wait; checking status…
login-auth-signed-out = Not signed in for this credentials store
login-auth-signed-in = Signed in: { $identity }
login-auth-plan-enabled = Plan inference permission: granted
login-auth-plan-disabled = Plan inference permission: not granted
login-auth-token-valid = Access token valid until { $time } (UTC)
login-auth-token-expiring = Access token expires at { $time } (UTC); it refreshes automatically on use
login-auth-token-expired = The locally recorded access token expired at { $time } (UTC); it refreshes automatically on use. Sign in again only if refresh fails
login-auth-token-unknown = Access token expiry unknown
login-auth-identity-mismatch = The saved credential does not match the verified account; choose “Use this account” to adopt it
login-auth-identity-required = ChatGPT plan credentials need a verified identity; sign in, check status, or use a verified account
login-auth-identity-checking = Checking sign-in status; a verified account will be applied automatically. Save after it completes
login-auth-identity-unchecked = Sign-in status not checked yet; check status or sign in to ChatGPT
login-auth-identity-signed-out = Not signed in for this credentials store; sign in to ChatGPT
login-auth-identity-incomplete = The verified account has no usable identity; sign in again
login-auth-identity-scope-changed = Sign-in status does not match the current credentials store/account; check status again
login-auth-identity-status-failed = Could not confirm sign-in status ({ $reason }); check status again or sign in
login-auth-identity-outcome-unknown = Sign-in result unknown; check status first
login-auth-credentials-missing = This provider keeps a ChatGPT reference, but the credentials store is not signed in; runtime will fail. Sign in again
login-auth-logging-out = Signing out…
login-auth-cancelling-logout = Stopping the remote revocation wait; local cleanup continues…
login-auth-local-cleared = Local credentials cleared
login-auth-remote-revoked = Remote grant revoked
login-auth-remote-unconfirmed = Remote revocation not confirmed
login-auth-logout-cleared-remote-revoked = Local credentials cleared; remote grant revoked
login-auth-logout-cleared-remote-unconfirmed = Local credentials cleared; remote revocation not confirmed
login-auth-logout-uncleared-remote-revoked = Local credentials were not cleared; remote grant revoked
login-auth-logout-uncleared-remote-unconfirmed = Local credentials were not cleared; remote revocation not confirmed
login-auth-outcome-unknown = Result unknown; verify status before assuming sign-in or sign-out
login-auth-unknown-query = Use “Check status” to verify
login-auth-error-storage = Credential store is unavailable or has unsafe permissions
login-auth-error-unavailable = ChatGPT authentication service is unavailable
login-auth-error-denied = ChatGPT sign-in was denied
login-auth-error-callback = Sign-in callback validation failed
login-auth-error-identity = Signed-in identity did not match this provider reference
login-auth-error-generic = ChatGPT authentication failed
login-auth-ended = Sign-in ended or was cancelled; start again
login-auth-platform-unsupported = ChatGPT plan credentials are not supported on this platform
login-auth-no-plan-scope = Signed in, but direct plan inference permission was not granted
login-auth-target-required = Set the credentials store first
login-auth-login = Sign in to ChatGPT
login-auth-status = Check status
login-auth-use-account = Use this account
login-auth-logout = Sign out
login-auth-cancel-logout-wait = Cancel sign-out wait
login-auth-cancel = Cancel sign-in
login-auth-title = ChatGPT sign-in
login-auth-url-label = Authorization URL
login-auth-loopback-local-hint = The callback is received on this machine (127.0.0.1); SSH or containers need port forwarding.
login-auth-open-browser = Open browser
login-auth-copy-url = Copy link
login-auth-browser-requested = Browser open requested
login-auth-browser-failed = Could not open the browser; copy the link instead
login-auth-copy-failed = Could not copy the link
login-auth-url-copied = Authorization link copied
login-auth-unsaved = ChatGPT credentials are saved, but the provider configuration is not saved yet
login-auth-credentials-saved = ChatGPT credentials saved
login-auth-account-adopted = Identity adopted from the verified account
login-auth-detail-hint = ↑/↓ select · Enter run · Esc cancel sign-in
login-auth-logout-title = Confirm ChatGPT sign-out
login-auth-logout-confirm = Sign out of the current ChatGPT credential? Providers sharing this credentials store: { $providers }
login-auth-logout-scope-note = Provider configuration is kept. Already-sent requests are not guaranteed to be recalled.
login-auth-cancel-logout = Cancel
login-auth-confirm-logout = Sign out
login-auth-logout-hint = ←/→ select · Enter run · Esc back (default: cancel)
login-auth-busy-save = Finish or cancel the authentication action before saving
login-auth-busy = Another authentication operation is still in progress
login-auth-wait-before-edit = Authentication is in progress; wait or cancel before changing the target
login-error-store-absolute = Credentials store must be an absolute path
login-error-account-reference = Invalid credential reference; check the configuration or sign in again
login-type-locked-hint = Saved type — press Enter to change
login-type-change-title =  /login — Change Provider Type
login-type-change-confirm = Enter: confirm type change · Esc: cancel
login-type-change-impact = Profiles bound to this provider ({ $profiles }) keep their bindings. Configure models and context windows in the Model panel.
login-type-change-none = none
login-save-summary = Save: provider { $id }
login-error-id-empty = Provider name cannot be empty
login-error-id-duplicate = Provider name already exists
login-error-profile-model-missing = Tier { $alias } has no usable model; fill the matching default model or set it in the Model panel
login-error-profile-window-missing = Tier { $alias } has no context window; set it in the Model panel
login-error-profile-provider-missing = Tier { $alias } has an unresolvable provider reference; reselect it in the Model panel
login-error-profile-resolver = { $alias } cannot be resolved: { $error }; configure it in the Model panel
login-warning-existing-unresolved = Saved, but these tiers were already unresolvable; fix them in the Model panel: { $aliases }
login-error-single-line = Single-line field: line breaks and control characters are not allowed
login-error-unknown-type = Unknown saved provider type; choose a supported type
login-error-authority = Configuration authority unavailable; reopen /login after checking the configuration source
login-error-provider = Provider validation failed: { $error }
login-error-provider-missing = Saved provider is no longer available; reload and reopen /login.
login-conflict-reload-hint = Press Ctrl+R to reload the latest configuration and discard this draft.

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
app-no-api-key-warning = Warning: No API Key set for the selected provider
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
app-compact-no-provider = Compact failed: No LLM Provider configured in settings
app-compact-compressing = Compressing context
app-compact-done = Context compressed
app-compact-failed = Compact failed: { $error }
app-compact-auto-cleared = Auto cleanup: freed { $count } tool call results
app-compact-limit-reached = Context still exceeds limit after compression. Use /compact to manually compress or /clear to clear history.
app-model-switched = Model switched to: { $alias } ({ $effort } effort)
app-1m-context-enabled = 1M context mode enabled (context window: 1,000,000 tokens)
app-prompt-cache-low = Prompt cache coverage { $rate }% < 80% (req: { $req })
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

# ---- Panel Confirm Target Guard ----

# 确认期间被确认的对象消失（退出确认时按稳定身份定位失败）；操作未执行。
app-confirm-target-missing = Confirm target no longer exists: { $target }

# Background Agent Bar
bg-bar-focus-hint = Press Esc to exit focus

# ---- Model Panel ----

model-panel-title =  Select model 
model-panel-selection-legend = ● active · ❯ selected
model-panel-description =   Switch between models. Applies to this session.
model-field-max-token = Max Token
model-field-effort = Effort
model-field-1m-context = 1M Context
model-effort-low = Low
model-effort-medium = Medium
model-effort-high = High
model-effort-xhigh = XHigh
model-effort-max = Max
panel-model-nav-hint =   ↑/↓::select  Enter::activate  Tab::side  →/←::value  Esc::exit/close
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
panel-agent-nav-hint =   ↑/↓::navigate  Enter::close  Esc::close

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
panel-hooks-nav-hint =   ↑/↓::navigate  Enter::close  Esc::close
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

thread-browser-title = Resume Session
thread-browser-search-placeholder = Search…
thread-browser-empty =   (No conversations yet)
thread-browser-no-match =   (No matching conversations)
thread-browser-untitled = (untitled)
thread-browser-time-just-now = just now
thread-browser-time-minutes = { $count } minute{ $suffix } ago
thread-browser-time-hours = { $count } hour{ $suffix } ago
thread-browser-time-days = { $count } day{ $suffix } ago
panel-threads-header-hint =   Enter::resume · v::read history · Esc::close
panel-threads-nav-hint =   ↑/↓::navigate  Enter::resume  v::read  Tab::scope  d::delete  Esc::close
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

# ---- Pasted image attachments (upload-style, no file name, numbered by paste order) ----
# $index is the attachment's position in the submitted batch (1-based); the composer
# label and the message-area record share the same numbering.
attachment-image-name = Image { $index }
# $size is a human-readable size (user-image-size-*).
user-attachment-image-meta = [Image] { $name } · { $size }

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
# Image attachments (pasted images): $name is `attachment-image-name`; multiple
# images add the count and the first few names.
composer-image-attachment = [Image] { $name }
composer-image-attachments = [Image] ×{ $count } · { $names }
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
paste-in-progress = Clipboard paste in progress, please wait
paste-image-failed = Could not paste image: invalid PNG or over 20 MiB
paste-terminal-hint = Cannot read this machine's clipboard here — paste with your terminal (Cmd+V / Ctrl+Shift+V)
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
reminder-legacy-marker = Legacy reminder
reminder-required-marker = Required reminder
reminder-structured-marker = System reminder
reminder-legacy-source = legacy history
reminder-relayed-from = Forwarded from subagent { $name } · handled by that subagent
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
panel-title-workflow = Workflow
panel-title-ask-user = Ask User
panel-title-theme = Theme
panel-title-subagent-detail = SubAgent Detail
panel-title-shell-detail = Shell Detail
panel-title-goal = Goal

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
panel-desc-workflow = Workflow runs
panel-desc-ask-user = Agent user questions (auto-open)
panel-desc-theme = Color theme selection
panel-desc-subagent-detail = Subagent nested transcript detail
panel-desc-shell-detail = Background shell task detail
panel-desc-goal = Current goal detail
goal-status-active = Active
goal-status-complete = Complete
goal-status-blocked = Blocked
goal-detail-status = Status
goal-detail-continuations = Proactive continuations
goal-detail-blocked-reason = Blocked reason
goal-detail-empty = No goal is available for this session.
subagent-detail-not-found = Subagent not found — the session may have been reset.
subagent-preview-omitted = Some temporary preview was released; saved child history remains available.
subagent-history-open-hint = h: View child session history (includes multiple runs)
subagent-history-title = Saved child session history — includes multiple runs
subagent-history-tool-error = Tool result (failed)
subagent-history-page-keys = [ / ]: Older / newer page · Tab: Select large entry · Enter: Read · r: Refresh · h/Esc: Preview
subagent-history-body-keys = n: Next chunk · p: Read from start · b: Back to page · h/Esc: Preview
subagent-history-loading = Reading saved history…
subagent-history-empty-page = No visible entries on this page. Use [ / ] to continue when available.
subagent-history-large-entry = Large saved entry
subagent-history-invalid-response = History response is unavailable or belongs to a different snapshot. Press r to reopen.
subagent-history-page-too-large = This page exceeds the display budget; it was not retained.
shell-detail-not-found = Shell task not found — it may have finished or the session was reset.
shell-detail-no-output = Output is not streamed to the TUI for running shell tasks.
shell-detail-running = Running…
shell-detail-output-preview = Output preview
shell-detail-status-running = Status: running
shell-detail-status-succeeded = Status: succeeded
shell-detail-status-failed = Status: failed
shell-detail-status-cancelled = Status: cancelled
bg-task-unknown-kind = Unknown background task type — drawer not opened.
bg-task-status-lost = connection lost
bg-task-status-reconciling = reconnecting
bg-task-status-running = running
bg-task-status-pending-delivery = delivery pending
bg-task-status-completed = completed
bg-task-status-failed = failed
bg-task-status-cancelled = cancelled
bg-task-status-unknown = unknown
workflow-run-not-synced = Selected workflow run is not in the current snapshot (finished or not synced yet).


# ---- Cron Panel ----
panel-cron-stats =   { $configured } configured, { $enabled } enabled
service-projection-startup = Showing startup plugins and hooks; session MCP and cron data require an active session.
service-projection-no-client = Service data is unavailable: no ACP client is connected.
service-projection-session-unavailable = Service data is unavailable while the session is changing or has not become active.
service-projection-failed = Service data could not be refreshed: { $detail }
panel-cron-action-failed = Cron operation failed: { $detail }
panel-cron-no-client = Cron operations require a connected ACP client.
panel-cron-no-session = Cron operations require an active session.
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
panel-mcp-server-detail =      transport: { $transport }  tools: { $count }  skills: { $skills }
panel-mcp-needs-auth =  [needs auth]
panel-mcp-list-hint =   Enter: details  |  Esc: close
panel-mcp-oauth-hint =   Enter: details  |  Esc: close
# MCP panel detail view (OAuth authorize entry)
panel-mcp-detail-url = URL:
panel-mcp-detail-cache = Cache:
panel-mcp-detail-protocol = Protocol:
panel-mcp-detail-connected = Connected:
panel-mcp-detail-cache-none = No cache activity yet
panel-mcp-detail-error = Error:
panel-mcp-detail-btn-auth = Authorize
panel-mcp-detail-btn-copy = Copy
panel-mcp-detail-btn-back = Back
panel-mcp-detail-copied = Error message copied
panel-mcp-detail-copy-failed = Copy failed: { $error }
panel-mcp-detail-hint =   ←→: select action  |  Enter: confirm  |  Esc: back to list
panel-mcp-oauth-start-failed = MCP authorization failed to start: { $error }
panel-mcp-oauth-start-no-session = MCP authorization failed to start: no active session
panel-mcp-icon-connected = ✔
panel-mcp-icon-error = ✗
panel-mcp-icon-unknown = ?
panel-mcp-cache-hit = CACHE hit
panel-mcp-cache-version-hit = CACHE version hit
panel-mcp-cache-protocol-hit = CACHE protocol hit
panel-mcp-cache-saved = CACHE saved
panel-mcp-cache-ready = CACHE ready
panel-mcp-cache-disabled = CACHE off: authenticated
panel-mcp-cache-disabled-config = CACHE off: configuration
panel-mcp-cache-pending = CACHE pending initialization
panel-mcp-cache-live-fetch = Persistent cache: live fetch
panel-mcp-cache-none = Persistent cache: —
panel-mcp-table-title = { $label } · { $count }
panel-mcp-table-tools = Tools
panel-mcp-table-skills = Skills
panel-mcp-table-agents = Agents
panel-mcp-table-subscriptions = Subscriptions
panel-mcp-table-col-name = Name
panel-mcp-table-col-description = Description
panel-mcp-table-col-item = Item
panel-mcp-table-col-value = Value
panel-mcp-table-empty = —
panel-mcp-table-more = … { $count } more
panel-mcp-sub-resources = Resources
panel-mcp-sub-tools-list-changed = tools/list_changed
panel-mcp-sub-prompts-list-changed = prompts/list_changed
panel-mcp-sub-resources-list-changed = resources/list_changed
panel-mcp-sub-on = on
panel-mcp-sub-off = off

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
panel-plugin-detail-hint = ↑/↓: select  |  Enter: execute  |  Esc: back

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
panel-plugin-errors-hint-keys = ←/→/Tab: switch view  |  Enter: back to installed  |  Esc: close

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
panel-plugin-search-invalid-response = Invalid search response
panel-plugin-operation-complete = operation complete
panel-plugin-operation-failed = operation failed
panel-plugin-operation-status = operation status
panel-plugin-management-unsupported = This plugin cannot be managed in an unambiguous writable installation scope.
panel-plugin-operation-wait-hint = Esc stops client waiting only
panel-plugin-operation-wait-cancelled = Client waiting stopped. Server outcome unknown; check state before retrying.
panel-plugin-operation-timed-out = Client wait timed out. Server outcome unknown; check state before retrying.
panel-plugin-operation-session-changed = Session changed. Server outcome unknown; check state before retrying.
panel-plugin-discover-press-enter = Press Enter to search
panel-plugin-action-install = Install
panel-plugin-list-count = { $count } plugins found
panel-plugin-discover-hint-keys = Enter: details  |  type: filter  |  ←/→/Tab: switch view

# ---- Plugin Panel Navigation ----
panel-plugin-list-hint = ←/→/Tab: switch view  |  ↑/↓: navigate  |  Enter: details  |  Esc: close

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
panel-tasks-nav-hint =   ↑/↓::navigate  Enter::cancel  Esc::close
panel-tasks-nav-hint-close =   ↑/↓::navigate  Enter::close  Esc::close
panel-tasks-cancel-no-session =   Cancellation not sent: no active session
panel-tasks-cancel-target-gone =   Cancellation not sent: the task is no longer in the list
panel-tasks-cancel-failed =   Cancellation failed: { $detail }

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
workflow-footer-shortcuts = Tab::next-run · Shift+Tab::prev-run · ←/→::pane · ↑/↓::navigate · Enter::kill-run · Esc::close
workflow-phases-header = Phases ({ $count } agents)
workflow-model-header = Model
workflow-kill-no-session = No active session; cannot stop the workflow run
workflow-kill-failed = Could not stop workflow run
workflow-kill-requested = Stop requested for run { $run }

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
# 风险选择的取消项：与具体风险种类无关，各风险说明共用。
risk-choice-cancel = Cancel (default)

popup-confirm-empty =   No pending confirmation.
popup-confirm-superseded =   The confirmation changed; review and confirm again.
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
app-note-compact-completed = { $type } completed { $detail }
app-note-compact-completed-summary = { $type } completed { $detail } — { $summary }
app-note-compact-detail = ({ $messages } messages, ~{ $tokens } tokens saved, { $files } files, { $skills } skills)
app-note-compact-detail-full = ({ $messages } messages, token saving unmeasured, { $files } files, { $skills } skills)
app-note-compact-error = Context compaction failed: { $message }
app-note-rewind-error = Rewind failed: { $message }
app-note-compact-type-full = Full compaction
app-note-compact-type-micro = Micro compaction
app-note-compact-type-smart = Smart compaction
app-note-agent-failed = Agent execution failed: { $message }
app-note-cache-hit-low = Prompt cache coverage {$pct}% < 80% (req: { $req_id })
app-note-cache-coverage-low = Prompt cache coverage {$pct}% < 80% — cached {$cached} / input {$input}, uncached {$uncached} (req: {$req_id})

# ---- Semantic tool cards ----

reminder-compact-file = File context
reminder-compact-skill = Skill instructions
reminder-compact-summary = Compaction summary

# Pending input queue
steer-queue-title = Pending { $count }
steer-input-rejected = Input was not accepted. Your draft has been kept.
steer-input-uncertain = Waiting for the input receipt. Retrying with the same input ID.
steer-session-unavailable = Session could not be established: { $error }. Your draft has been kept.
steer-fatal-not-sent = Execution failed. { $count } unsent input(s) restored to the composer.
steer-fatal-uncertain = Execution failed. The result of { $count } input(s) is unknown; the drafts remain in Pending for reconciliation.
steer-fatal-mixed = Execution failed. { $not_sent } input(s) restored to the composer; { $uncertain } input(s) have unknown results and remain in Pending.
steer-item-uncertain = unconfirmed

thread-browser-project = Project
thread-browser-workspace = Workspace
thread-browser-count = { $count } sessions loaded
thread-browser-count-more = { $count } sessions loaded · more available
thread-browser-filter-label = Filter:
thread-browser-status-label = Status:
thread-browser-status-active = Active
thread-browser-messages = { $count } messages
panel-threads-more-hint =   ↑/↓::browse (loads more at end)  PgUp/PgDn::page  Tab::scope  n::more  d::delete
thread-browser-all = All
thread-browser-selected = Selected: { $title }
thread-browser-selected-path = Path: { $path } · ID: { $id }
thread-browser-actions = ↑↓ select  Enter resume  v view  Tab scope  a archive/restore  r archived view  d delete  Esc close
thread-browser-actions-compact = Enter resume  a toggle  r archived  d delete  Esc close
thread-browser-archived = Archived
thread-browser-delete-error = Could not delete session
thread-browser-archive-error = Could not update archive status
thread-browser-machine = { $name } ({ $id }) · { $count } workspaces
thread-browser-preview-actions = v back  ↑↓ scroll  PgUp/PgDn page  Esc close
thread-history-preview-hint = Read-only history · ↑/↓::scroll · v::back · Esc::close
thread-history-loading = Loading history…
thread-history-disconnected = History is unavailable while disconnected.
thread-history-user = User
thread-history-assistant = Assistant
thread-history-system = System context
thread-history-tool = Tool result
session-restore-failed = Session restore failed: { $error }. Retry or use /clear to create a session.
session-creation-failed = Session creation failed: { $error }

panel-host-settings = Host settings

permission-mode-update-failed = Permission mode update failed: { $error }
setup-activation-failed = Settings were saved, but could not be activated. Press Enter to retry.
setup-saving = Saving and activating settings…
setup-connectivity-checking = Checking endpoint…
setup-connectivity-invalid = Enter a valid HTTP(S) URL without embedded credentials.
setup-connectivity-failed = Endpoint check failed or timed out. Check the URL and network.
setup-connectivity-reachable = Endpoint reachable; API key and model not verified.
setup-connectivity-status = Endpoint returned HTTP { $status }; API key and model not verified.
setup-migration-failed = Could not find a valid provider in ~/.claude/settings.json. Choose Custom API to enter one.
setup-provider-incomplete = Select at least one provider and complete its ID, API key, and model fields.
setup-field-id-readonly = ID (read-only)
msg-history-tool-result-missing = Incomplete history: no tool result recorded. Execution was not resumed.
shell-detail-status-unobserved = Status: no current execution observed

# ---- Exit hints ----

exit-resume-hint = To resume this session: peri -r { $session }

panel-title-history-detail = History Detail
panel-desc-history-detail = Oversized history entry detail

# ACP MCP runtime control
panel-mcp-detail-btn-restart = Restart
panel-mcp-detail-btn-stop = Stop
panel-mcp-restarting = Restarting MCP… Esc stops waiting.
panel-mcp-stopping = Stopping MCP… Esc stops waiting.
panel-mcp-control-connected = MCP connected.
panel-mcp-control-stopped = MCP stopped.
panel-mcp-control-needs-auth = MCP needs authorization. Choose Authorize to continue.
panel-mcp-control-failed = MCP operation failed: { $detail }
panel-mcp-control-unknown = MCP result unknown. Check the refreshed status before retrying.
panel-mcp-control-target-unavailable = This MCP server is no longer available for that action.
panel-mcp-control-unavailable = MCP runtime control is unavailable.
panel-mcp-control-builtin = Builtin MCP servers are managed by the host.
panel-mcp-control-workspace = The workspace connection is managed by the host.
panel-mcp-control-system = System MCP servers are managed by the host.
panel-mcp-control-acp = MCP over ACP does not support runtime control.
panel-mcp-control-session-policy = MCP runtime control is disabled for this session.
