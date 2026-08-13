# acp-hub UI/UX audit

- Status: Open
- Date: 2026-08-13
- Scope: `acp-hub/web` interaction, visual system, component primitives, accessibility
- Method: source review plus read-only desktop/mobile inspection of `http://127.0.0.1:8456/`

## Verdict

第一阶段建立了正确的数据边界和 ChatGPT 式两栏轮廓，但当前产品仍更像 ChatGPT 的截图，而不是一套可靠的 agent 交互系统。下一阶段应先修状态机与移动端结构，再谈视觉抛光。

## P0 — release blockers

### UX-P0-001 Sending can lose the draft and create an unknown duplicate

`Composer` clears the textarea before `sendMessage` knows whether the action was accepted. A WebSocket race or 30-second Ack timeout only produces a transient toast. The user loses the original text and may retype a command that the server later executes anyway.

- Evidence: `web/src/panel/components/Composer.tsx`, `web/src/panel/store.ts::sendAction`, `sendMessage`
- Required direction: durable client outbox states (`sending`, `accepted`, `uncertain`, `committed`, `failed`); preserve the draft; retry the same `commandId`; describe timeout as “result unknown”.
- Verify: fault injection before send, after accepted, timeout, and late committed Ack; exactly one server execution.

### UX-P0-002 A running agent cannot be stopped from the UI

`cancelTurn` and `closeChat` exist in the store but no visible component calls them. A runaway or mistaken agent action cannot be interrupted from the primary surface.

- Evidence: `web/src/panel/store.ts::cancelTurn`, `closeChat`; `web/src/panel/components/Composer.tsx`
- Required direction: replace Send with Stop while a turn is active; expose runtime close separately; show pending/failure state.

### UX-P0-003 Permission decisions can be submitted more than once

Allow and Deny remain enabled until the Yjs projection updates. A double click or Allow→Deny sequence can submit conflicting security decisions.

- Evidence: `web/src/panel/components/MessageList.tsx::PermissionBar`, `web/src/panel/store.ts::resolvePermission`
- Required direction: lock by `permissionId` after the first decision, set both buttons busy/disabled, and unlock only on terminal error.

### UX-P0-004 Mobile dialogs are not viewport-level overlays

Dialogs are descendants of the transformed mobile drawer. CSS fixed positioning is therefore constrained by the drawer; at 390px the overlay covered only the left 320px and leaked the page on the right.

- Evidence: `ProjectSidebar.tsx`, `AppShell.tsx`, `.project-drawer`, `.ui-dialog-backdrop`
- Required direction: Portal every application overlay to a shared body-level overlay root; test dialogs under transformed ancestors.

### UX-P0-005 Logout collides with the mobile composer

The fixed bottom-right logout control occupies the same mobile hot zone as Send.

- Evidence: `AuthGate.tsx`, `.logout-button`
- Required direction: move account/logout into the sidebar footer or an account menu; never float it over the primary task.

## P1 — core usability

1. **Fatal connection states have no recovery action.** Replace contradictory green footer/header states with one source of truth and persistent, actionable reconnect/login/instance-offline banners.
2. **Refresh does not restore the last working session.** Persist and restore the last logical session, showing an explicit restoring state.
3. **Import candidates are unsafe to identify.** System-reminder content and raw ISO timestamps reach the UI; add cleaned titles, local relative time, first-user-message summary, ID suffix, preview and explicit confirmation.
4. **The empty state blocks the product's central action.** It hides the Composer and sends the user to two unlabeled sidebar icons. Keep the Composer available and create/bind a session on first send, or provide direct New/Import actions.
5. **Sessions are indistinguishable.** Multiple “新对话” rows have no summary, relative time, or runtime state. Auto-title from the first user message and add restrained secondary metadata.
6. **The rename “Menu” has no menu/popover behavior.** Add trigger relationships, focus entry/return, Escape/outside dismissal and collision-aware positioning; treat it as an edit popover, not a menu.
7. **Errors exist for only 2.5 seconds.** Keep success toasts short, but attach errors and uncertain outcomes to the relevant action/session with Retry, Copy details and Dismiss.
8. **Agent output is not a coding reader.** Add safe Markdown, code language/copy controls, links, and collapsible tool inputs/results/duration/errors.
9. **Streaming accessibility is noisy.** Do not put the token-updating message tree in a live region; announce start/completion, permissions and errors once.
10. **Small muted text and focus rings miss contrast.** Split decorative and essential metadata tokens; meet 4.5:1 for small text and 3:1 for focus indicators.

## P2 — system quality

1. Consolidate the three competing styling paths: `ui/*` primitives, semantic global classes and ad-hoc Tailwind utilities.
2. Turn `Dialog`, `Menu`, `Button`, `ListItem`, `TextArea`, `StatusBadge` into behavioral primitives, not tag wrappers.
3. Remove near-duplicate hard-coded colors, radii and shadows; enforce tokens.
4. Make mobile touch targets at least 44×44 and never hide essential actions behind hover.
5. Add “new messages / jump to bottom” when auto-follow pauses.
6. Correct the active Composer placeholder; it currently still says a conversation must be selected.
7. Add compact/medium/wide layouts instead of treating responsiveness as only “hide the sidebar”.

## Recommended execution order

1. **Reliability and safety:** draft outbox, Stop, permission lock, persistent errors.
2. **Overlay and mobile shell:** Portal, logout relocation, drawer background inertness, touch targets.
3. **Recovery and information architecture:** last session, unified connection state, session state labels.
4. **Import and chat readability:** metadata cleaning, preview, Markdown/code/tool rendering.
5. **Component-system convergence:** behavioral primitives, tokens, responsive contracts and accessibility tests.

## What should be preserved

- Sidebar catalog includes only hub-created or explicitly imported sessions.
- Session opening is command-correlated and ignores late Acks.
- User messages are restrained and assistant text remains on the reading surface.
- Dialog focus loop, IME Enter protection, reduced motion and read-only double gating are solid foundations.
- Permission Allow is not styled green, avoiding visual pressure on a security decision.

## Implementation progress

### 2026-08-13 — P0 reliability and mobile shell

- `UX-P0-001`: implemented command-correlated message submission states. Draft text survives sending, accepted, uncertain and failed states; retry reuses the original `commandId`; late committed/duplicate Ack clears uncertainty.
- `UX-P0-002`: Composer Send becomes Stop while the projected turn is active; cancel has busy, error and unknown-result handling.
- `UX-P0-003`: permission decisions lock by `permissionId` after the first click and remain locked until the server projection removes the request; an explicit error unlocks safe retry.
- `UX-P0-004`: Dialog now portals to a body-level overlay. Mobile inspection at 390×844 measured the overlay at `x=0,width=390,height=844`.
- `UX-P0-005`: logout moved into the sidebar footer and no longer overlaps the mobile Composer.
- Unified text-field focus styling with the Composer's neutral visual language and raised the global focus indicator to a solid high-contrast token.

Verification:

- `cd acp-hub/web && bun run test` — 9/9 passed.
- `cd acp-hub/web && bun run build` — passed.
- `git diff --check` — passed.
- The restarted isolated service recovered 137 chats, completed instance reconciliation and serves `index-DWymwBYd.js` with `index-CX79VPkU.css`.
- The final refinement also replaces the session-row overflow glyph with an explicit edit icon because the action only renames; its visual symbol, accessible name and result now agree. The latest isolated service serves `index-CfJu9m0O.js` with `index-CX79VPkU.css`.
- Browser checks: desktop shell, 390×844 drawer, full-viewport Dialog overlay and footer logout placement.

### 2026-08-13 — P1 recovery and session discovery

- Fatal connection states now produce persistent, actionable recovery panels. Authentication invalidation returns to login; instance/heartbeat failures retain the catalog and offer reconnect.
- WebSocket retries now preserve exponential backoff across failed attempts and reset only after a real `ready` frame or an explicit user connection.
- The last logical project session is remembered as a UI preference. Restoration validates the server catalog, reopens through `session/open`, and never treats the cached id as authoritative runtime state.
- Import candidates strip system reminders, use local relative times and an ACP id suffix, and require selection plus explicit confirmation. The dialog remains open until committed/duplicate Ack.
- Sidebar connection status now uses the same live source as the chat header instead of always claiming a safe connection.
- Session rename now behaves as an edit dialog: focus enters the field, Escape or outside click closes it, focus returns to the trigger, and trigger/panel ARIA relationships are explicit.
- Streaming tokens are no longer exposed through a live message tree. A dedicated atomic status announces only terminal assistant outcomes; animated dots are hidden from accessibility APIs.
- Essential muted text now uses a darker semantic token, and session action targets were enlarged from 28px to 36px without changing row density.
- Server-projected tool arguments, results and public errors now survive Yjs normalization and render in keyboard-accessible, collapsed tool disclosures. No duration or hidden data is fabricated.
- Server action errors now remain in a bounded in-context error center with Copy details and Dismiss controls instead of disappearing with the 2.5-second success-toast lifecycle.
- Assistant terminal messages now use a dependency-free, token-based Markdown reader. It supports headings, lists, quotes, emphasis, safe links, inline/fenced code and copy controls; raw HTML remains inert text and active URL protocols are rejected. Streaming stays plain text to avoid repeated full-document parsing and half-fence layout churn.
- Long conversations now pause auto-follow when the user scrolls upward, retain a deterministic new-content signal, and expose an always-reachable floating “new content / back to bottom” action.
- Empty state now exposes direct New project, New session, Import or Select project actions instead of routing users toward unlabeled sidebar icons.
- Sidebar session rows now show persisted relative activity and the logical/runtime state (`未启动`, `正在恢复`, `可输入`, failure/reconciliation states), making duplicate fallback titles distinguishable.
- Solid primitives now strip component-only props before DOM forwarding, own explicit label/description relationships, share Copy feedback, and coordinate nested overlay inert state per application root.
- The former generic `Menu` has been replaced by a semantic edit `Popover` with focus entry, Escape/outside dismissal and trigger focus restoration; coarse-pointer targets share a 44px minimum interaction size.
- Login now explains the default token file and exact full-token generation command, while 401, hostile origin, throttling, server failure and transport failure receive different guidance. A server failure is never mislabeled as a bad credential.
- The selected conversation header now separates the persisted session from its runtime: a keyboard-operable action menu can close only the current runtime after an explicit explanation/confirmation, while terminal sessions offer a safe reopen path. Close has pending, committed, failure and unknown-result UI states.
- ACP `session/list` title changes now update SQLite by exact durable session id and reproject the sidebar. Empty titles cannot erase known metadata, user aliases remain authoritative, unchanged polls do not churn generations, and an unprojected SQLite generation is retried until Registry converges.
- Added a real jsdom component harness covering busy/disabled semantics, DOM prop isolation, field accessibility, clipboard success/failure, Dialog and Popover focus lifecycles, inert release, safe Markdown link isolation, and fenced-code controls. A real Y.Doc fixture also verifies tool arguments/results/public errors through the normalization boundary.

Verification:

- `cd acp-hub/web && bun run test` — 18/18 Node contracts and 10/10 Solid DOM/Yjs tests passed, including restore eligibility, import presentation, authentication error classification, unsafe Markdown protocols/raw HTML, paused-follow behavior, overlay focus lifecycles and menu keyboard navigation.
- `cd acp-hub/web && bun run build` — passed (76 modules; 162.30 kB JavaScript before gzip).
- `cargo test -p acp-hub-server --lib persist::metadata_test` — 13/13 passed; exact/idempotent title refresh, alias preservation, generation-consistent snapshots and monotonic projection watermarks included.
- `cargo test -p acp-hub-server --lib channel::command_coordinator::command_coordinator_test` — 35/35 passed; strengthened `session/list` integration proves ACP title persistence.
- `cargo test -p acp-hub-server --lib control::project_service::tests::title_refresh_repairs_an_existing_projection_gap` — passed; a prior SQLite→Registry projection failure converges on an unchanged later poll.
- `cargo test -p acp-hub-server --lib` — 362/362 passed with loopback permissions; gateway, auth, Hub, persistence, projection and HTTP tests included.
- `cargo clippy -p acp-hub-server --lib -- -D warnings` — passed.
- `git diff --check` — passed.
- Browser checks: a selected logical session remained the same sidebar row after a full reload, and its Composer was restored without creating or importing a session.
- A final post-build browser reload was blocked by the browser URL safety policy after the local server restart; no bypass was attempted. Critical keyboard and overlay behavior is now covered by executable jsdom interaction tests.

### 2026-08-13 — Registry recovery performance and bounded storage

- Startup no longer decodes the Registry append log three times. `StoreSink` performs one recovery pass, caches one merged state update for `DocManager`, and caches the stale-runtime reconciliation set.
- Registry persistence is now a verified snapshot plus bounded incremental log. Crossing 8 MiB materializes only the current visible map graph into a fresh Yjs document, eliminating obsolete CRDT items and tombstones without losing additive/unknown nested fields.
- Snapshot publication is crash-safe: private tmp file, full fsync, atomic rename, directory fsync, CRC/Yjs read-back validation, then log rotation. Partial/corrupt log tails are physically truncated to the last complete record; a corrupt snapshot fails startup.
- The first legacy-log migration preserves the complete previous log as `registry.log.legacy-v1` with mode `0600`. The production data directory was deliberately not migrated without separate user authorization.

Evidence from an isolated byte-for-byte copy of the current production data:

- Legacy Registry: 223,006,900 bytes / 160,744 records; prior observed startup to listen was about 34 seconds.
- First migration: 10.3 seconds to decode legacy history, then a 97,623-byte semantic snapshot; original 213 MiB log retained as rollback backup.
- Second cold recovery: 2.12 seconds to rebuild 137 chat mirrors and Registry state (binding was intentionally denied by the sandbox after recovery).
- Semantic-history test rewrites one visible key 2,000 times and proves the materialized state is at least 20× smaller while preserving the current scalar and an unknown nested extension map.

Verification:

- `cargo test -p acp-hub-server --lib` — 366/366 passed with loopback permissions.
- `cargo clippy -p acp-hub-server --lib -- -D warnings` — passed.
- Registry recovery/compaction tests cover permissions, reopen, pre-truncation crash idempotence, corrupt-tail truncation and semantic-history removal.

### 2026-08-13 — Project navigation and reversible lifecycle

- Project headings are now real disclosure controls with session counts; the primary `+` remains one click away while import and lifecycle actions move into a keyboard-navigable project menu.
- Projects can be renamed through a focused dialog. The additive `project/rename` command changes only display metadata and proves project id, cwd, instance binding and session identities remain intact across the SQLite→Registry commit barrier.
- Import discovery now includes project context, an explicit non-destructive explanation, and title/session-id search before confirmation.
- Project removal is a reversible product flow rather than a one-way UI action: confirmation explains that sessions and workspace files remain, archived projects move to a dedicated collapsed collection, and users can restore them in place.
- A project with any non-terminal runtime cannot be archived. The UI disables the action with a direct instruction, while the server independently verifies every persisted session binding and fails closed if metadata cannot be read; an agent can never continue invisibly behind an archived navigation group.
- Added the additive `project/restore` wire action. Restore participates in the same persistent commandId dedup, SQLite generation, Registry projection barrier, ordered accepted/committed Ack and read-only authorization as archive.
- Session overflow controls now use the shared SVG icon language instead of text glyphs; project action noise is reduced until hover/focus while touch targets remain explicit on coarse pointers.

Verification:

- `cargo test -p acp-hub-proto` — 34/34 unit and 5/5 contract tests passed with the additive action and allowlist.
- `cargo test -p acp-hub-server --lib` — 368/368 passed, including metadata archive/restore retention and ordered coordinator Ack roundtrip.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cd acp-hub/web && bun run test && bun run build` — 18/18 Node contracts, 10/10 DOM/Yjs tests and production build passed.

### 2026-08-13 — Solid component library enforcement

- Added behavioral `Status` and bounded auto-growing `Textarea` primitives, replacing duplicated connection semantics in header/sidebar and hand-written Composer resizing.
- All feature components now import through `src/ui/index.ts`; implementation-file deep imports are forbidden by an executable source architecture test.
- Added `src/ui/README.md` as the component contract: ownership boundaries, DOM-only prop filtering, promotion criteria, token rules, destructive-flow responsibility and required real-component tests.
- Status tones are a closed TypeScript union shared with connection state, so an unknown visual semantic cannot silently render as an unstyled string.
- Component DOM suite now covers 12 behavioral tests, including Status live/tone semantics and Textarea prop isolation/controlled input.

Verification:

- `cd acp-hub/web && bun run test` — 19/19 state/architecture contracts and 12/12 real Solid DOM/Yjs tests passed.
- `cd acp-hub/web && bun run build` — passed (79 modules, 168.17 kB JavaScript before gzip).

### 2026-08-13 — P0 regression barriers and global session search

- The five P0 fixes are now executable contracts rather than review notes. Message uncertainty preserves source text and command identity through late committed/duplicate Acks; Stop remains the active-turn primary action; permission locks are idempotent; Dialog must remain portaled outside the transformed app tree; logout cannot return as a fixed Composer overlay.
- Added global persisted-session search across active projects with NFKC matching for title, project, cwd and durable ACP id. `⌘/Ctrl+K` opens the palette and ArrowUp/ArrowDown traverse enabled results.
- Fixed two invalid session-search design tokens and verified that every CSS custom-property reference resolves to a declared token.
- The latest server runs against `/private/tmp/acp-hub-semantic-bench.8NlLAm`, an isolated copy. A runtime audit found that `dev.sh` previously isolated only the server: `acp-instance` still opened its default data directory and ran residual-process cleanup. `dev.sh` now passes `${ACP_HUB_DATA_DIR}/instance` explicitly so both processes share the same isolation boundary. No formal server metadata migration was performed; the earlier instance-side touch is recorded rather than described as untouched.

Verification:

- `cd acp-hub/web && bun run test` — 23/23 Node state/architecture contracts and 12/12 Solid DOM/Yjs tests passed.
- `cd acp-hub/web && bun run build` — passed (81 modules; 172.51 kB JavaScript before gzip).
- `cargo test -p acp-hub-proto` — 34/34 unit and 5/5 contract tests passed.
- `cargo test -p acp-hub-server --lib` — 372/372 passed with loopback permissions.
- Auth contract 4/4, integration 6/6 and resilience 3/3 passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `git diff --check` and CSS custom-property scan — passed.
- A fresh 390px browser screenshot was not obtained: the in-app browser rejected local URL access under its URL safety policy. No bypass or alternate automation surface was used; Portal ancestry and inert behavior remain covered by real DOM interaction tests.

### 2026-08-13 — High-frequency component convergence

- `Button` now owns a closed visual contract for primary, secondary, ghost and danger actions plus compact/default sizing. Busy state always couples `disabled`, `aria-busy`, one spinner and one accessible processing label.
- Composer Send/Stop, message submission recovery actions, Permission Allow/Deny, jump-to-latest and sidebar logout now consume the Solid UI primitives instead of carrying independent native-button state and near-duplicate classes.
- Permission pending state now records the submitted decision, not merely the request id. The interface says `正在允许…` or `正在拒绝…`, keeps both decisions locked, and cannot be visually ambiguous about which safety choice is in flight.
- Link, composer-border, focus-border, translucent surface and scrollbar literals were promoted into semantic tokens. Feature TSX files are now forbidden from introducing literal colors.
- Added architecture tests that forbid raw buttons in Composer/MessageList and literal colors in all feature components; these are deliberate high-frequency boundaries, not a blanket ban on native semantics inside behavioral primitives.

Verification:

- `cd acp-hub/web && bun run typecheck && bun run test && bun run build` — 23/23 Node contracts and 13/13 real Solid DOM/Yjs tests passed; production build passed.
- `git diff --check -- acp-hub/web` — passed.

### 2026-08-13 — Shared compact viewport contract

- Responsive behavior now has one exported `compactViewportQuery` rather than a JavaScript/CSS split-brain. AppShell and CSS both use the guarded 959px boundary.
- The layout now has a real compact state for tablets and narrow notebooks: navigation becomes a drawer, connection detail collapses, Composer and reading width tighten, and persistent error surfaces keep usable margins. Phone-specific safe-area and stacked recovery actions remain below 480px.
- The responsive source contract forbids feature-local numeric media queries and verifies the CSS/TypeScript boundary together.

Verification:

- `cd acp-hub/web && bun run typecheck && bun run test && bun run build` — 26/26 Node contracts and 13/13 Solid DOM/Yjs tests passed; production build passed (82 modules).
- `git diff --check -- acp-hub/web` — passed.

### 2026-08-13 — First-message quick start

- With exactly one active project, the empty conversation surface now accepts the first message directly. The user no longer has to create an empty session, wait, rediscover it and then type.
- Quick start remains server-authoritative: it first sends durable `session/create`, waits for a complete committed/duplicate Ack containing both logical session and runtime chat ids, selects/subscribes that exact runtime, then submits the preserved prompt.
- Session creation uncertainty retains project, source text and the original commandId. Retry reuses the same frame, so server metadata dedup prevents duplicate logical or ACP sessions. A definite failure can return to editing; an uncertain result cannot be dismissed into an unsafe new create.
- The first line seeds the initial persisted title (bounded to 60 characters) while later exact ACP title refresh and user alias rules remain unchanged.
- With multiple active projects, the quick-start surface exposes an explicit, accessible project selector before submission. With no active project it offers project creation; archived projects are never valid ownership targets. The shortcut never guesses durable ownership.
- The shared `SelectField` owns label, hint, error and `aria-describedby` relationships without allowing descriptive text to pollute the control's accessible name. If the chosen project disappears before submission, the form safely falls back to the first active project.
- Navigation semantics now expose the selected logical session with `aria-current`, and global search options report their real `aria-selected` state. `MenuItem` centralizes menu roles, safe button type and danger tone rather than repeating fragile feature markup.
- JavaScript helper declarations now carry explicit submission and Markdown token structures instead of using broad `any` return types at the `.mjs` boundary.

Verification:

- `cd acp-hub/web && bun run typecheck && bun run test && bun run build` — 27/27 Node contracts and 17/17 Solid DOM/Yjs tests passed; production build passed (85 modules; 178.28 kB JavaScript before gzip).
- Quick-start contracts cover wrong command, missing runtime identity, late committed, duplicate and non-retryable failure.
- Real Quick Start component tests preserve a drafted first message across multi-project selection and safely recover when the selected active project disappears.
- Runtime probe against the isolated local service returned unauthenticated `401` with `Cache-Control: no-store`, CSP, `nosniff`, `no-referrer` and frame denial headers. The served entry referenced the latest `index-CpFZG-cV.js` and `index-CphMgX72.css` build assets.

### 2026-08-13 — Metadata mutation reliability

- Project/session forms now retain their user input and busy state until a matching committed/duplicate terminal Ack; a timeout or definite failure unlocks without performing destructive UI cleanup.
- Disconnect settles every pending action through its action-specific uncertainty path instead of discarding timers and callbacks. Replaced WebSocket epochs cannot mutate the current connection state.
- Empty-session creation is one store-level single-flight across project `+`, empty state, quick start and failed-session replacement.
- Metadata uncertainty keeps the exact original action frame and commandId. New metadata writes are blocked until the original request is safely confirmed; retries reuse the same identity and cannot be double-submitted while pending.
- Recovery cards that gate metadata writes are never evicted by the five-item ordinary-error bound, including the case where more than five operations become uncertain in one disconnect.
- `dev.sh` now passes the selected data root to `acp-instance` as well as server and exposes `ACP_HUB_INSTANCE_DATA_DIR` for a fresh verification-only watermark directory. This closes the local isolation leak without pretending to solve the pre-existing M1 process-group reuse risk documented in `acp-hub/docs/plans/f6-machine.md`.

### 2026-08-13 — Trustworthy tool execution reader

- Tool calls are now a dedicated Solid feature component rather than an inline raw dump. The collapsed summary carries stable identity, localized lifecycle, restrained status tone and an optional elapsed-time fact; the expanded reader separates input, output and public error with independent copy controls.
- Added additive `startedAt` and `completedAt` tool projection fields. The start comes from the existing ACP timestamp or Hub observation fallback; completion uses the Hub receive clock. The UI labels the interval `Hub 观测` and suppresses it for legacy, malformed or negative timestamps instead of claiming an ACP-native execution metric.
- Timestamp state survives tool updates because aggregator upserts begin from the current projection. Existing snapshots remain compatible through optional serde/Yjs fields.
- Tool payloads remain inert: nested Yjs data becomes plain values, Solid writes text nodes, and a DOM regression proves markup-shaped error text cannot create an element. The existing server result-size budget remains authoritative; absent terminal output is described as empty-or-omitted rather than silently presented as success.
- Error cards open by default; ordinary/running cards remain collapsed to preserve conversation scanability. Data viewers are bounded and scrollable without adding a heavy syntax-highlighting dependency.

Verification:

- `cargo test -p acp-hub-proto` — 35/35 unit and 5/5 contract tests passed.
- `cargo test -p acp-hub-server --lib` — 372/372 passed with loopback permissions.
- Focused normalization/projection tests prove completion stamping, timestamp preservation and oversized-result behavior.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cd acp-hub/web && bun run test && bun run build` — 32/32 Node contracts, 26/26 Solid DOM/Yjs tests and production build passed (87 modules).

### 2026-08-13 — Stable fallback session identity

- Default titles no longer collapse multiple persisted rows into indistinguishable `新对话` / `未命名会话` labels. Only these low-information fallbacks gain a stable ACP-or-Hub id suffix; meaningful user/ACP titles remain visually untouched.
- The secondary line continues to carry runtime lifecycle and relative activity time, so identity, readiness and recency are available in one scan without adding badge clutter.
- The display helper is pure and covered by the state contract suite. It does not alter persisted metadata, search identity or the catalog rule that only Hub-created/explicitly-imported sessions appear.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 32/32 Node contracts, 26/26 Solid DOM/Yjs tests and production build passed (87 modules).

### 2026-08-13 — Explicit tool-result provenance

- Tool completion projection now distinguishes three facts: Hub explicitly retained/received no output, Hub explicitly omitted an over-budget output, and an old snapshot has no provenance. The tri-state survives serde and Yjs replay rather than collapsing legacy uncertainty into false certainty.
- Oversized results no longer become an unexplained null. The Hub stores no content, but records the compact JSON byte count and `result_omitted=true`; the execution reader says `输出未载入` with the observed size and projection-limit explanation.
- Tool execution state is now server-authoritative end to end: official ACP `tool_call_update(in_progress)` advances the existing call instead of being dropped as a duplicate start; linked permissions project `awaitingPermission`, allow resumes running, and deny/expiry cancel without allowing late ordinary updates to reopen or regress the tool.
- Explicitly empty results say the tool returned no displayable output. Legacy nulls say the old projection cannot establish whether output was empty or omitted. Public errors and omission provenance render independently.
- No result prefix, hash, hidden error, resource URL or credential is retained. This improves truthfulness without weakening the existing 4096-byte public projection boundary.

Verification:

- `cargo test -p acp-hub-proto` — 35/35 unit and 5/5 contract tests passed.
- `cargo test -p acp-hub-server --lib` — 373/373 passed with loopback permissions.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cd acp-hub/web && bun run test && bun run build` — 32/32 Node contracts, 29/29 Solid DOM/Yjs tests and production build passed (87 modules).
- In-app browser inspection was attempted; its local tab remained at `无法访问此站点`, so no screenshot/pixel claim is made.

### 2026-08-13 — Composer visual hierarchy

- The highest-frequency surface no longer reads like an operator/debug panel. Model, effort and context facts collapse into one quiet runtime identity; complete values remain available through the accessible label and hover title without competing with authored text.
- Empty, opening, read-only, terminal, working and submission-uncertain placeholders now describe the actual next safe action. In particular, an unselected conversation no longer invites typing and then explains parenthetically that typing is unavailable.
- Keyboard behavior is visibly discoverable on layouts with room for it (`Enter` send, `Shift + Enter` newline) and disappears on compact screens before crowding the primary action.
- The surface uses one neutral border, a restrained neutral focus halo and a lighter elevation. There is no blue focus border, internal toolbar divider, or second competing input outline.
- Send and stop keep one stable 40–44px circular target at the trailing edge; mobile safe-area padding and touch sizing remain intact.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 34/34 Node contracts and 32/32 Solid DOM/Yjs tests passed; production build passed (88 modules; 187.27 kB JavaScript before gzip).
- `git diff --check` — passed.
- Latest screenshot/pixel validation remains an explicit evidence gap because the in-app browser security policy rejects localhost automation; no visual claim is inferred from source tests alone.

### 2026-08-13 — Durable session vs runtime clarity

- The conversation header no longer collapses every non-terminal state into `保存在项目中`. One pure state contract now distinguishes a saved-but-not-started session, ACP restoration, an input-ready runtime, active Agent work, a pending permission, normal end, explicit close and abnormal process exit.
- Persistence and process state remain separate in the language: every terminal label explicitly says that the runtime ended while the durable session remains saved. Reopen therefore reads as a runtime action, not recovery of deleted chat data.
- Pending permission takes precedence over generic Agent activity, and abnormal exit receives the only danger treatment. Healthy input readiness is quiet; it does not compete with the independent transport status.
- Compact layouts retain this safety-critical status with truncation instead of hiding the entire subtitle. Reduced-motion users inherit the existing global animation shutdown.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 35/35 Node contracts and 35/35 Solid DOM/Yjs tests passed; production build passed (89 modules; 188.11 kB JavaScript before gzip).
- The sidebar and header now consume the same state contract. Pure-state coverage includes unstarted, ready, awaiting-permission, reconciliation and crashed states; real ChatHeader DOM coverage proves durable identity, permission precedence and crash copy/tone.

### 2026-08-13 — Safe component-library form semantics

- The shared `Button` now defaults to native `type="button"`. Reusable actions can no longer submit an enclosing form merely because a feature is later rearranged inside one.
- Form ownership remains explicit: login, project creation and rename/save actions declare `type="submit"`; cancel, retry, copy, permission and navigation actions stay non-submitting.
- The component contract and library README record this invariant. A real DOM regression clicks default and explicit-submit buttons inside the same form and proves only the latter emits submit.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 36/36 Node contracts and 36/36 Solid DOM/Yjs tests passed; production build passed (89 modules; 188.48 kB JavaScript before gzip).
- `git diff --check` — passed.

### 2026-08-13 — Design-token ownership boundary

- Reusable design values now have one component-library source: `web/src/ui/tokens.css`. The panel stylesheet retains feature selectors and responsive ordering, so the extraction does not perturb Tailwind processing or cascade precedence.
- Remaining empty-state colors, form/auth/recovery elevations and semantic error/warning borders were promoted from feature literals into named tokens. `styles.css` now contains no literal color declarations.
- Architecture tests prove the Tailwind→token import order, forbid a second `:root` token source, reject feature-level literal colors, and verify that every `var(--token)` referenced by feature CSS is declared.

Verification:

- `cd acp-hub/web && bun run test` — 41/41 Node state/architecture contracts and 47/47 Solid DOM/Yjs tests passed, including the final token-literal and declaration-completeness guards.
- `cd acp-hub/web && bun run build` — passed (91 modules; 189.88 kB JavaScript and 42.70 kB CSS before gzip).
- `git diff --check` — passed.
- The restarted local service serves the matching `index-DuOvEUcw.js` and `index-DeBTwXHU.css` assets; unauthenticated auth status correctly returned 401.

### 2026-08-13 — Responsive navigation primitive

- Compact navigation is now a behavioral `Drawer` primitive rather than an AppShell-specific keyboard handler. Wide layouts remain ordinary structural navigation; compact open state alone receives dialog semantics, a labeled scrim, focus entry/trap/return and background inertness.
- Drawer participates in the same overlay stack as Dialog. Opening a project Dialog from mobile navigation makes it the sole Escape owner; the first Escape closes the Dialog and the second closes navigation.
- Menu and Popover now join that stack without making the background inert. A project menu consumes its own Escape before Drawer, and returning to wide layout clears stale modal-open state so a later compact transition cannot unexpectedly reopen navigation.
- `AppShell` now owns only responsive state and product routing. An architecture contract forbids it from reacquiring document-level keyboard or inert behavior.

Verification:

- `cd acp-hub/web && bun run test` — 42/42 Node state/architecture contracts and 51/51 real Solid DOM/Yjs tests passed, including compact/wide Drawer semantics plus nested Dialog/Menu ordering.
- `cd acp-hub/web && bun run build` — passed (92 modules; 190.55 kB JavaScript and 42.97 kB CSS before gzip).
- `git diff --check` — passed.
- The restarted isolated service recovered 137 chats, authenticated the local instance, completed alive-session reconciliation, and serves `index-DfiMhPiz.js` with `index-mqK1ud5z.css`.

### 2026-08-13 — Badge ownership and dead-token removal

- Message status inference remains a feature-domain adapter, but the visual badge is now a closed Solid UI primitive with neutral/success/warning/error tones and prop isolation. Feature code no longer owns Tailwind color recipes for a reusable control.
- Removed obsolete three-column language and unused right-rail/layout tokens left behind by the old operator dashboard. A whole-source scan now reports zero undeclared tokens and zero unused tokens.
- The token completeness contract now scans feature TSX custom-property references as well as CSS, preventing utility-class token usage from bypassing the design-system boundary.

Verification:

- `cd acp-hub/web && bun run test` — 43/43 Node state/architecture contracts and 52/52 real Solid DOM/Yjs tests passed.
- `cd acp-hub/web && bun run build` — passed (93 modules; 190.52 kB JavaScript and 43.05 kB CSS before gzip).
- Full token scan: `unused=[]`, `undeclared=[]`; `git diff --check` passed.
- In-app browser visual validation was attempted through its supported control surface, but localhost remained blocked by the browser URL safety policy. No alternate automation surface or policy workaround was used; screenshot evidence remains explicitly unavailable.
- The restarted isolated service recovered 137 chats, reauthenticated the local instance and serves the final `index-Cb3idMXb.js` / `index-d7TZ8Xc5.css` pair.

### 2026-08-13 — Visible icon help without clipping

- `IconButton` now composes a shared Tooltip instead of relying on native `title`. Keyboard focus reveals help immediately; pointer hover uses a restrained delay; Escape, blur and pointer leave dismiss it.
- Tooltip content is supplemental visual help, not part of the control's accessible name. Disabled icon controls remain hover-explainable while enabled controls retain their exact `aria-label` action name.
- Tooltip renders through a body Portal and anchors with fixed viewport coordinates, so Composer overflow, mobile Drawer transforms and Header clipping cannot cut it off. Start/center/end placement plus automatic above-edge flipping keep high-frequency controls inside the viewport.
- Project, navigation, session-menu, Send and Stop icon controls explicitly choose the safe edge alignment instead of relying on one global center assumption.

Verification:

- `cd acp-hub/web && bun run test` — 44/44 Node state/architecture contracts and 57/57 real Solid DOM/Yjs tests passed, including delayed hover, keyboard/Escape, activation dismissal, touch-hover suppression, non-duplicating accessible descriptions, Portal ancestry, end alignment and viewport-edge flipping.
- `cd acp-hub/web && bun run build` — passed (94 modules; 192.51 kB JavaScript and 43.79 kB CSS before gzip).
- `git diff --check` — passed.

### 2026-08-13 — Commit-gated session navigation

- Global search and mobile sidebar no longer dismiss navigation immediately after sending `session/open`. Selection and navigation close only after the same command receives committed/duplicate with a runtime chat id.
- Search keeps the original query and target visible while opening. Definite failure is shown inline; timeout says the result is unknown and explicitly confirms that the current conversation did not switch.
- Search results use the same stable fallback-title contract as sidebar rows, so repeated `新对话` sessions remain distinguishable by durable id suffix.
- Store-level `openProjectSession` now exposes optional committed/failed/uncertain callbacks while preserving existing callers. Protocol ordering remains in the store; components own only their surface lifecycle.

Verification:

- `cd acp-hub/web && bun run test` — 45/45 Node state/architecture contracts and 63/63 real Solid DOM/Yjs tests passed. SessionSearch retains query and stable fallback identity across committed, definite-failure and uncertain-timeout flows; ProjectSidebar separately proves committed-only navigation, failure retention and read-only reuse of an existing runtime.
- `cd acp-hub/web && bun run build` — passed (94 modules; 193.22 kB JavaScript and 44.00 kB CSS before gzip).
- An architecture contract prevents sidebar/search navigation from returning to send-then-close behavior; `git diff --check` passed.
- The isolated service recovered 137 chats, authenticated/reconciled the local instance, and serves `index-CLoomQby.js` with `index-CWruQ5aU.css`.

### 2026-08-13 — First-prompt session identity

- A Hub-created session no longer remains one of several indistinguishable fallback rows while waiting for an ACP title refresh. After the prompt has crossed the ACP dispatch boundary and the server has attempted its authoritative user-entry projection, the Hub derives a one-shot navigation fallback from its first meaningful line.
- The fallback is a separate SQLite fact (`hub_title`), introduced by an additive v2→v3 migration. It is not written into `acp_title` and never claims that the ACP thread was renamed.
- Display precedence is explicit and tested: user alias → meaningful ACP title → Hub first-prompt fallback → low-information ACP default → `新对话`. A later exact-id `session/list` title naturally takes over; a user alias always remains authoritative.
- Derivation normalizes internal whitespace, ignores leading blank lines, truncates on Unicode character boundaries at 60 characters, and writes only once for `origin='hub'`. Imported and unknown ACP sessions are never admitted or renamed by this path.
- A metadata/projection failure is logged without changing the already-safe prompt delivery result. The next title refresh/reprojection remains the repair path; naming cannot turn a delivered prompt into a false failure.

Verification:

- `cargo test -p acp-hub-server --lib` — 390/390 passed with loopback permissions, including additive migration, title precedence/idempotency and the prompt-forward→SQLite→Registry coordinator path.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` — passed in the standalone acp-hub workspace.
- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node contracts, 63/63 Solid DOM/Yjs tests and production build passed (94 modules; 193.22 kB JavaScript and 44.00 kB CSS before gzip).

### 2026-08-13 — Project session row boundary

- The project sidebar no longer implements a saved session's open, read-only reuse, rename form and recovery affordances inline. `ProjectSessionRow` is now a feature-domain component with an explicit facts/events interface; it has no WebSocket, Yjs subscription or command-id knowledge.
- Navigation remains server-authoritative: a normal row delegates the navigation callback as the `session/open` committed continuation, while a read-only principal may only switch locally to a runtime already proven active by Registry state.
- Rename draft, validation and submitting state are local to the row. The editor closes only after committed/duplicate acknowledgement and remains actionable after a definite send or server failure.
- Architecture contracts were updated to follow the actual component boundary rather than assuming these controls remain textually inside `ProjectSidebar.tsx`.

Evidence:

- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node contracts and 66/66 Solid DOM/Yjs tests passed across 11 test files; production build passed (95 modules; 194.11 kB JavaScript and 44.00 kB CSS before gzip).
- `cd acp-hub/web && git diff --check` — passed.

### 2026-08-13 — Session import workflow boundary

- `SessionImportDialog` now owns the import query, candidate selection and submitting lifecycle. `ProjectSidebar` supplies only the selected project fact, current ACP candidates, close event and authenticated import command.
- Candidate admission remains exact and inspectable: cwd scope is applied before presentation, already-cataloged ACP ids are excluded by the store selector, and search matches the cleaned title or full stable ACP session id.
- Import is confirmation-driven. The dialog remains open while the command is pending, closes only after committed/duplicate acknowledgement, and preserves the selected candidate after a definite failure so the user can retry without reconstructing context.

Evidence:

- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node contracts and 69/69 Solid DOM/Yjs tests passed across 12 test files; production build passed (96 modules; 194.30 kB JavaScript and 44.00 kB CSS before gzip).
- Focused DOM coverage proves cwd filtering, stable-id filtering, committed-only close and failure-state retention.

### 2026-08-13 — Browser command lifecycle module

- The 900-line Solid store no longer owns ad-hoc pending timers and uncertain-retry maps. `CommandTracker` is the single in-process module for transport acceptance, accepted→terminal ordering, timeout, connection-loss settlement, exact-frame retry and late-terminal cleanup.
- Its interface is deliberately domain-neutral: callers provide one frame, one label and lifecycle callbacks; the module knows nothing about projects, sessions, prompts, permissions, Solid signals or UI copy. The store remains the adapter that maps generic outcomes into domain state.
- `accepted` never releases a command. Exactly one pending terminal acknowledgement or error releases its timer and continuation; a late terminal acknowledgement after timeout clears reconciliation evidence without re-running the expired continuation.
- Retryable metadata uncertainty retains the exact original frame and command id until terminal evidence or explicit dismissal. A transport-rejected retry cannot erase that evidence, and a duplicate concurrent dispatch with the same command id never reaches the socket twice.
- Connection replacement, reconnect, fatal close, ordinary close and explicit disconnect all pass through the same settlement path. Reset cancels every timer and clears both pending and reconciliation state without firing domain callbacks.

Evidence:

- Direct fake-timer tests cover accepted→committed, exactly-once terminal handling, timeout→same-frame retry, late terminal cleanup, disconnect settlement, transport rejection, duplicate in-flight dispatch, failed retry evidence retention, terminal error and reset cleanup.
- Store architecture contracts now follow the module seam instead of asserting a particular private map/helper implementation.
- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node contracts and 78/78 module/Solid DOM/Yjs tests passed across 13 test files; production build passed (97 modules; 195.69 kB JavaScript and 44.00 kB CSS before gzip).
- `cd acp-hub/web && git diff --check` — passed.

### 2026-08-13 — Compact / medium / wide reading rhythm

- Responsive behavior now has three explicit product states instead of treating every non-mobile viewport as the same desktop. Compact (`≤959px`) keeps the proven modal Drawer; medium (`960–1199px`) keeps navigation structural but reduces it from 280px to 240px; wide (`≥1200px`) retains the full catalog and 820/864px reading rhythm.
- The medium state spends reclaimed width on the actual task: message reading is capped at 760px and the Composer at 800px, while header, alert and content gutters tighten together. Navigation hierarchy remains intact; project/session actions and runtime state are not hidden or replaced by an icon-only rail.
- Breakpoint ownership remains centralized in `ui/breakpoints.ts`. `MEDIUM_VIEWPORT_MAX=1199` joins the existing compact and phone contracts; feature JavaScript contains no duplicated media-query literal.
- The medium media block is deliberately CSS-only because no interaction semantics change there. Drawer modal state still begins only at the compact threshold, preventing a second responsive state machine.

Evidence:

- The source contract proves all three ranges, the 240px structural sidebar, 760/800px reading limits, and the absence of fixed/modal Drawer behavior in medium mode.
- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node contracts and 78/78 module/Solid DOM/Yjs tests passed; production build passed (97 modules; 195.70 kB JavaScript and 44.60 kB CSS before gzip).
- `cd acp-hub/web && git diff --check` — passed.
- The restarted local service recovered 137 chats, reauthenticated the local instance, completed alive-session reconciliation and serves `index-CxnjoQ_A.js` with `index-DxM2Up69.css`. In-app-browser reload was explicitly rejected by its localhost URL policy; no workaround was attempted, so screenshot/pixel evidence for the medium state remains open rather than inferred from source.

### 2026-08-13 — Browser project/session restart journey

- The product's primary persistence claim is now covered by a real process-level integration journey rather than inferred from metadata unit tests. It starts the production server, instance daemon and ACP child, logs in through `POST /api/auth/session`, and opens a cookie-preauthenticated WebSocket whose first frame is the Registry subscription rather than a bearer token.
- The journey creates a project and Hub-owned logical session, sends a first prompt, then kills and restarts both server and instance against the same isolated data directories. The pre-restart browser cookie is required to fail because browser sessions are memory-only; a fresh login restores the same project id, logical session id and exact ACP session id from Registry v2.
- Reopening the restored logical session must return a new runtime chat id and the original ACP session id. The test then subscribes the new chat and requires both a committed prompt result and a Yjs chat update, proving the restored binding accepts real ACP traffic rather than merely acknowledging `session/load`.
- The ACP test child now models `session/load` explicitly: it adopts the requested durable session id and uses that exact identity for subsequent notifications. A focused child-process test prevents the E2E substitute from silently treating load as an unknown no-op.
- Login evidence also asserts `HttpOnly`, `SameSite=Strict`, `Path=/`, `Cache-Control: no-store`, `nosniff`, no bearer-token reflection, logout cookie expiry and post-logout 401.

Evidence:

- `cargo test -p acp-instance --test child_test test_session_load_restores_notification_identity` — passed.
- `cargo build --workspace` — passed.
- `cargo test -p acp-hub-server --test product_flow_tests -- --nocapture` — passed with real loopback processes; the final journey including post-load Yjs delivery finished in 2.04s.
- Full regression after the new journey: server lib 390/390, auth contracts 4/4, existing process integration 6/6, resilience 3/3 and product flow 1/1 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` and `git diff --check` — passed.
- `cd acp-hub/web && bun run typecheck && bun run test && bun run build` — 45/45 Node contracts, 78/78 module/Solid DOM/Yjs tests and production build passed (97 modules; 195.70 kB JavaScript and 44.60 kB CSS before gzip).
- Exact process inspection found only the intentionally retained local 8456 development server and its instance; no E2E `test-child` or temporary stack remained.

### 2026-08-13 — Closed browser-auth HTTP surface

- The browser token exchange is now a deliberately closed HTTP/1.1 protocol rather than permissive parsing added to the static asset server. POST and DELETE require an exact same-origin Origin; POST requires a non-empty bounded JSON body, while GET and DELETE reject bodies.
- Ambiguous framing fails closed: any Transfer-Encoding, duplicate security-relevant header, malformed header name, unsupported HTTP version, invalid Content-Length, undeclared trailing bytes, short body or oversized body is rejected before credential validation. The login JSON schema rejects unknown fields, so malformed requests cannot be mislabeled as bad credentials.
- `application/json` is the only login media type; one optional UTF-8 charset parameter is accepted. Form posts, missing content type, alternate charsets and duplicate parameters return 415/400 rather than reaching token validation.
- Browser Cookie lifetime and server-side session lifetime now share one Rust constant. Login sends `Max-Age=28800`, and every auth response carries `Cache-Control: no-store`, `Pragma: no-cache` and `nosniff`.
- The parser keeps one absolute five-second deadline across fragmented header/body reads. A real socket test proves legitimate fragmentation still reaches credential validation while the rejection matrix remains deterministic.

Evidence:

- `cargo test -p acp-hub-server --lib web::web_test` — 15/15 passed, including real TCP framing, Origin, media-type, body-boundary and fragmented-read cases.
- `cargo test -p acp-hub-server --lib` — 394/394 passed.
- `cargo test -p acp-hub-server --test auth_contract_tests` — 4/4 passed.
- `cargo test -p acp-hub-server --test product_flow_tests` — passed; the cookie-authenticated create/restart/load journey remained intact.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- The rebuilt isolated 8456 service returned 401 with `no-store`/`no-cache`/`nosniff`, 415 for `text/plain` login and 400 for chunked login in live black-box probes.

### 2026-08-13 — Interaction primitives under composition

- Every feature-owned native button now declares its form behavior, and the source contract rejects an untyped `<button>`. Component rearrangement can no longer reintroduce implicit submit outside the shared primitive.
- `TextField` and `SelectField` now share the same hint/error contract: invalid state, descriptive ids and visible error copy are owned by the field primitive. Session rename no longer hand-wires an unrelated error node.
- Coarse-pointer sidebars no longer hide 44px actions behind hover. Project actions and per-session menus are fully visible on touch; the selected session also keeps its action visible for mouse and keyboard users.
- Modal overlays now use a stack-aware lease rather than only a global inert count. Escape and focus trapping belong to the topmost Dialog; closing an inner Dialog preserves the outer Dialog and application inertness until the final release.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 38/38 Node contracts and 38/38 Solid DOM/Yjs tests passed; production build passed (89 modules; 188.97 kB JavaScript before gzip).
- Real nested-Dialog coverage proves one-Escape/one-layer dismissal and inert persistence.
- Running service recovered 137 chats, authenticated the local instance, completed replay, and served `index-D-kSoiyH.js` with `index-CVo3bqA9.css`.

### 2026-08-13 — Notification, token and Composer contracts

- Short-lived notifications are now a real `ToastViewport` primitive. The store still owns expiry, while the library owns polite live-region semantics, stable record rendering, responsive placement, entry motion and reduced-motion behavior. Feature code no longer carries a one-off Tailwind component implementation.
- Visually equivalent values now resolve through tokens: focus halos, danger/warning borders, white surfaces and the translucent header no longer drift through near-duplicate literals. This is deliberately semantic consolidation, not an unverified palette rewrite.
- Composer behavior is now proven in real Solid DOM tests, not only source regexes. Coverage verifies the truthful disabled placeholder before selection, empty-message gating, Send→Stop replacement during an active turn, disabled editing while active, and uncertain-message text/recovery preservation.

Verification:

- `cd acp-hub/web && bun run test` — 38/38 Node contracts and 43/43 Solid DOM/Yjs tests passed across 9 test files.
- `cd acp-hub/web && bun run build` — passed (90 modules; 188.72 kB JavaScript and 42.02 kB CSS before gzip).
- `git diff --check` — passed.

### 2026-08-13 — Trustworthy overlays, diagnostics and shortcuts

- Dialog now expresses in-flight ownership with a first-class non-dismissible state. Escape and backdrop clicks cannot imply cancellation while project/session mutations or runtime close are still awaiting a terminal result.
- Dialog's overlay lease restores the host's original inert value instead of always forcing it false. Search uses the primitive's optional visible title and explicit close button, while domain dialogs can retain their custom explanatory headers without duplicate chrome.
- Browser diagnostics no longer emit raw WebSocket data, server error frames, close reasons, Yjs exception objects or serialized action errors. Logs retain only protocol type, payload length, stable error code and command-presence metadata.
- Search shortcut labels are platform truthful: macOS displays `⌘K`, while Windows/Linux display `Ctrl+K`; the actual key handler continues to accept either primary modifier.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 40/40 Node contracts and 46/46 Solid DOM/Yjs tests passed; production build passed (91 modules; 189.81 kB JavaScript and 42.48 kB CSS before gzip).
- Tests cover non-dismissible mutation state, visible Dialog title/close action, original inert restoration, payload-safe diagnostics and platform shortcut labels.

### 2026-08-13 — Single-owner session navigation

- Logical session restore, explicit open, read-only live-runtime reuse, failure, uncertainty and late terminal acknowledgement are now one `SessionNavigator` state machine. The Solid store adapts effects to protocol sends, local preference cleanup and runtime Doc subscription; feature components no longer decide which navigation mechanism is safe.
- Runtime selection remains unchanged until the exact current `session/open` command returns `committed` or `duplicate` with a chat id. An unrelated terminal Ack cannot supersede the active request, and a terminal Ack arriving after timeout cannot move the UI.
- Late terminal Acks are no longer discarded before `CommandTracker`. They can close durable uncertainty evidence while the navigator independently quarantines their UI continuation. This removes the previous conflict between transport reconciliation and stale-navigation safety.
- Last-session recovery runs once per authenticated UI lifetime, accepts only a Registry-proven ready session, reuses a live runtime in read-only mode, and forgets a stale browser preference without treating localStorage as authority.
- The legacy `open-state.mjs` reducer and its duplicated source-contract tests were deleted. Module-level tests now exercise the public state-machine interface, including failed restore non-repetition.

Verification:

- `cd acp-hub/web && bun run test` — 41/41 Node architecture contracts and 84/84 module/Solid DOM/Yjs tests passed across 14 files.
- `cd acp-hub/web && bun run build` — passed (97 modules; 197.61 kB JavaScript and 44.60 kB CSS before gzip).

### 2026-08-13 — Finite icon and Composer layout contracts

- A real authenticated browser pass exposed a severe visual regression that source-only review had missed: the session-search circle inherited SVG's default black fill and no finite size, producing a large black disk in the sidebar. Every feature-owned icon now supplies geometry through the shared `Icon` primitive, which owns the 20×20 canvas, closed sizes, outline paint, line caps and decorative accessibility semantics.
- An architecture contract scans every feature component and rejects bare `<svg>` canvases. This prevents a new button or navigation entry from silently bypassing the component system and relying on a selector that happens to include its class.
- The same narrow-screen pass found that Tooltip's wrapper, not the nested Send button, participates in Composer flex layout. The toolbar now assigns the auto margin to `.ui-tooltip-anchor`, keeping Send/Stop pinned to the bottom-right hot zone on compact screens.

Verification:

- `cd acp-hub/web && bun run test` — 42/42 Node architecture contracts and 85/85 module/Solid DOM/Yjs tests passed across 14 files.
- `cd acp-hub/web && bun run build` — passed (98 modules; 198.11 kB JavaScript and 44.74 kB CSS before gzip).
- Authenticated desktop browser evidence confirms the former black disk is now an 18px outline search icon and all sidebar/project/session controls share one stroke language.
- Authenticated 390×844 evidence confirms Drawer/search Dialog remain viewport-correct and the Composer action geometry is `left=333,right=373,bottom=828` inside a surface `left=10,right=380,bottom=834`.

### 2026-08-13 — Standalone primitive visual boundary

- `src/ui/primitives.css` is now the component library's public visual entry and imports `tokens.css` itself. A consumer no longer needs product `styles.css` to render Button, Icon, Field, Dialog, Drawer scrim, Popover, Menu, Tooltip, Badge, Status, Spinner, Toast, EmptyState or shared scrollbars correctly.
- Product `styles.css` retains only shell/feature layout and parent-qualified contextual overrides. Drawer scrim ownership moved into the Drawer primitive, removing a hidden dependency on the product's compact media query.
- An executable architecture contract requires the public import chain, proves all base selectors exist in `primitives.css`, and rejects independent primitive-base redefinitions in product styles while still permitting explicit contextual layout overrides.

Verification:

- `cd acp-hub/web && bun run test` — 43/43 Node architecture contracts and 85/85 module/Solid DOM/Yjs tests passed across 14 files.
- `cd acp-hub/web && bun run build` — passed (98 modules; 198.12 kB JavaScript and 45.06 kB CSS before gzip).

### 2026-08-13 — Runtime hydration truth and empty-conversation continuity

- Authenticated desktop/mobile inspection found a selected, writable, zero-message session rendered as a nearly blank page: Header already said `可输入`, Composer was active, but the conversation surface gave no confirmation that session activation had succeeded. More importantly, the same empty entry array also existed before runtime history arrived, so adding generic empty copy alone would have lied during restore.
- The store now tracks chat and control Y.Doc hydration independently. Runtime selection resets both facts; each fact becomes true only after the matching server update is applied. Composer remains disabled and Header/message surface say `正在载入会话` until both are authoritative.
- A projected pending permission remains higher priority than ordinary loading status as soon as the control document arrives. Once both documents are present and the server projection is truly empty, the conversation explains that the first message will remain attached to this persisted session.
- Routine `ready` no longer produces a success Toast. Connection health remains in the persistent Header/sidebar Status, so login and reconnect do not cover mobile navigation or the current-session identity with redundant feedback.

Verification:

- `cd acp-hub/web && bun run test` — 44/44 Node architecture/state contracts and 89/89 module/Solid DOM/Yjs tests passed across 15 files.
- `cd acp-hub/web && bun run build` — passed (98 modules; 199.07 kB JavaScript and 45.82 kB CSS before gzip).
- Final service recovered 139 chats, marked the stale runtime ended, authenticated the local instance and completed alive-session reconciliation.
- Authenticated 390×844 browser evidence shows the confirmed empty-session explanation, enabled Composer only after hydration, zero Toast overlays, and assets `index-Cs9nb9Sg.js` / `index-CP8RW9x0.css`.
- Authenticated 1280×720 evidence shows the empty explanation centered above the 824px Composer with `toastCount=0`; Header and selected sidebar row agree on `可输入 · 会话已保存` only after hydration.

### 2026-08-13 — Session-scoped Composer drafts

- Composer text is now keyed by persistent project-session identity. Switching runtimes cannot carry a draft into another logical conversation, while returning to the source session restores the exact unsent text.
- Message submission state carries command, project-session and runtime-chat identities. Transport failure or uncertain delivery restores text only to its owner session; another session receives a privacy-safe global single-flight notice without the source text.
- A user cannot dismiss an uncertain submission and accidentally reissue it under a new command. Definite failure can return the preserved text to editing, and a late terminal acknowledgement clears only the matching restored copy.
- Logout and authentication invalidation clear all in-memory drafts together with the server-authoritative UI session.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 44/44 Node contracts and 91/91 module/Solid DOM/Yjs tests passed across 15 files; production build passed (98 modules; 200.48 kB JavaScript and 45.89 kB CSS before gzip).
- Component tests cover A→B→A draft isolation/recovery and ensure another session's pending state never renders the source text.
- Authenticated browser evidence used two persisted sessions without sending: the target session's Composer stayed empty, both returns restored `A session local draft — do not send`, and the test text was cleared afterward.

### 2026-08-13 — Fact-grounded session import review

- Import now has three explicit phases: choose a cwd-scoped ACP candidate, review the exact known facts, then submit. The review exposes the title, local relative activity, project cwd and full ACP session id instead of relying on a truncated row alone.
- The UI states that ACP currently provides no message-content preview. It does not turn a title into a fabricated excerpt or imply that the conversation body has been inspected.
- A selection that disappears after search or catalog refresh cannot be submitted. Search and candidate controls remain locked while the server is deciding the import.
- Definite server rejection and delivery-unknown timeout are separate UI states. Only the latter tells the user to reconcile using the original request identity; both preserve the selected candidate and dialog context.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 44/44 Node architecture/state contracts and 94/94 module/Solid DOM/Yjs tests passed across 15 files; production build passed (98 modules; 202.34 kB JavaScript and 47.27 kB CSS before gzip).
- `bun run typecheck` and `git diff --check` passed.
- Six focused Dialog tests cover cwd filtering, commit-gated close, fact-only review semantics, stale/hidden selection invalidation, definite failure and delivery uncertainty.
- The restarted server recovered 142 chats, marked two stale runtimes ended, authenticated the local instance and completed reconciliation. A direct HTTP response serves the new `index-hwpuOODF.js` / `index-CIXr7V0A.css` bundle.
- The existing in-app browser tab retained its prior HTML/bundle after normal and hard refresh, so no claim is made that the new visual layout was observed there; behavioral DOM evidence comes from the real Solid component harness rather than a stale screenshot.

### 2026-08-13 — Upgrade-safe static cache contract

- Static HTTP caching is now classified by resource identity. App entry documents, compatibility routes, misses and fixed-name assets remain `no-store`, so a restarted server cannot intentionally bootstrap an obsolete Web bundle.
- Only a real embedded `/assets/` resource whose final stem contains a Vite-style fingerprint of at least eight safe characters receives one-year `immutable` caching. A path prefix or file extension alone is insufficient.
- Security headers are composed independently from cache policy. HTML, immutable assets and errors all retain CSP, `nosniff`, frame denial and no-referrer protection; authentication responses retain their stricter `no-store` plus `Pragma` contract.
- Static `HEAD` now mirrors the exact GET status, content metadata, cache policy and security headers without emitting a body. Deployment probes no longer report the existing app entry as 404.

Verification:

- `cargo test -p acp-hub-server --lib web::web_test::` — 18/18 passed, including real loopback GET/HEAD responses for entry HTML, fingerprinted assets and missing assets.
- `cargo test -p acp-hub-server --lib` — 396/396 passed across auth, gateway, persistence, metadata, Yjs projection and Web HTTP behavior.
- `cargo clippy -p acp-hub-server --lib -- -D warnings` and `cargo fmt --all -- --check` passed.

### 2026-08-13 — Restart journey proven at the ACP wire

- The browser project-session product journey now proves more than matching Ack fields: after killing and restarting server and instance, the fresh ACP process records the exact `session/load.params.sessionId` it received on stdin. The test requires that single value to equal the durable ACP id created before restart.
- The same journey still proves memory-only browser-cookie invalidation, re-login, SQLite/Registry catalog recovery, cleared runtime hints, a distinct fresh `chat_id`, continued prompt delivery/Yjs update, and logout revocation.
- Production redaction remains unchanged. Instance stderr handling still counts bytes/lines without logging bodies; only the fake ACP fixture accepts `--audit-file`, and it writes one structured method/session identity record inside the test's private temp directory.
- The integration fixture now creates the audit file eagerly and rejects a sibling `test-child` binary older than its source with an actionable `cargo build --workspace` diagnostic. This prevents stale fixture binaries from producing misleading product-test failures.

Verification:

- `cargo build --workspace && cargo test -p acp-hub-server --test product_flow_tests -- --nocapture` — passed; exact durable ACP id observed once across `session/load` stdin, then restored prompt committed and projected.
- `cargo test -p acp-instance` — 51/51 library tests and 8/8 child-process integration tests passed.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` and `git diff --check` passed.

### 2026-08-13 — Message delivery uncertainty has one owner

- `CommandTracker` is now the sole owner of prompt and quick-start retry identity. Both actions opt into retained uncertainty, and their recovery controls call `commands.retry(commandId, sendFrame)` instead of replaying a second store-owned frame cache.
- Only timeout or disconnect can expose “使用同一请求重新确认”. A definite `action_error` restores the original text for editing, marks the request non-retryable, and never reuses the terminal command id.
- The lifecycle test covers `sent → accepted → timeout → same-frame retry → duplicate`, asserts the exact frame identity, and proves a second terminal frame cannot settle the command twice. A separate regression proves terminal errors never enter the uncertainty map.
- Composer and quick-start keyboard focus now use a neutral high-contrast double ring. Pointer focus stays quiet, and the obsolete blue halo does not return. The shared field removes the duplicate global outline so its computed focus style has one intentional ring.

Verification:

- `cd acp-hub/web && bun run test && bun run build` — 45/45 Node architecture/state contracts and 97/97 module/Solid DOM/Yjs tests passed across 15 files; production build passed (98 modules; 201.94 kB JavaScript and 47.65 kB CSS before gzip).
- Focused lifecycle/Composer suite passed 19/19.
- The restarted local server loaded the final bundle. Browser computed style for the focused login field was neutral `rgb(98, 98, 94)`, with a white separation ring plus 3px neutral ring and no outline; the final CSS bundle also contained the Composer keyboard-focus rule.
- `git diff --check` passed.

### 2026-08-14 — Source CSS is parsed as a contract, not best-effort text

- The three authored stylesheets now pass Lightning CSS with error recovery disabled before PostCSS inspects their structure. A malformed media block or parser warning therefore fails the architecture suite instead of relying on the production bundler to recover silently.
- The AST contract forbids declarations directly under media queries and verifies that every consumed custom property is declared by the shared token source.
- Composer and quick-start now have one shared neutral `focus-within` owner. The obsolete duplicate focus rules and stale permission selector are gone; the blue treatment is limited to the high-contrast `focus-visible` keyboard ring rather than a persistent input border.
- `postcss` and `lightningcss` are explicit test dependencies, so the gate does not depend on an incidental transitive dependency from Vite.

Verification:

- `cd acp-hub/web && bun run test` — 51/51 Node architecture/state contracts and 131/131 module/Solid DOM/Yjs tests passed across 20 files.
- `cd acp-hub/web && bun run build` — production build passed at 101 modules: `index-BFfeTiFX.js` is 206.03 kB and `index-8ASp6MzR.css` is 48.02 kB before gzip.

### 2026-08-14 — Permission decisions remain safe when delivery is unknown

- The permission prompt is now an explicit `PermissionRequestCard` rather than anonymous markup inside the message list. It renders only server-projected facts: title, description and the associated tool identity; it does not invent an authorization scope that ACP did not provide.
- A decision has two client phases: `pending` while its terminal result is awaited, and `uncertain` after timeout or disconnect. Both phases disable Allow and Deny together. The uncertain state explains that the original decision may already have taken effect and forbids submitting the opposite choice.
- A definite `action_error` is the only client outcome that releases the decision lock for retry. A committed/duplicate acknowledgement does not eagerly unlock it; the server-authoritative permission projection must remove the request first.
- The card exposes `aria-busy` only during active delivery, uses a one-shot alert for uncertainty, and gives coarse-pointer/mobile actions a 44px target. Read-only principals see the request facts but no enabled mutation.

Verification:

- Focused permission/MessageList tests passed 6/6, covering known facts, first-decision dispatch, pending lock, uncertain lock and explanation, read-only closure, and runtime hydration.
- `cd acp-hub/web && bun run test && bun run build` — 46/46 Node architecture/state contracts and 101/101 module/Solid DOM/Yjs tests passed across 16 files; production build passed (99 modules; 203.52 kB JavaScript and 48.46 kB CSS before gzip).
- The architecture contract proves timeout transitions through `markPermissionDecisionUncertain`, while explicit errors retain the only unlock path.
- `git diff --check` passed.

### 2026-08-14 — Downstream JSON cannot crash the WebSocket callback

- The browser parser no longer casts every successful `JSON.parse` result to a frame. It accepts only a non-array object with a non-empty string `t`; `null`, arrays, primitives and missing/non-string/blank tags are malformed and never reach `ws-client`'s `frame.t` access.
- The parser deliberately does not embed the Rust frame registry. An unknown string tag remains a structurally valid envelope and reaches the store's compatibility default, where an older Web client safely ignores it. Shape validation therefore does not turn optional protocol evolution into a hard disconnect.
- Malformed-frame diagnostics remain payload-safe: the browser records only the received string length, never the raw frame, error body, token or user content.
- Repository evidence confirmed that normal server protocol failures use the typed `action_error` frame. The legacy `error`/`auth_error` checks in `ws-client` are compatibility/bootstrap handling, not a newly invented public frame schema.

Verification:

- Focused parser tests passed 10/10: known frame, unknown future tag, invalid JSON, null, array, string/number primitives, missing tag, non-string tag and blank tag.
- `cd acp-hub/web && bun run test && bun run build` — 47/47 Node architecture/state contracts and 111/111 module/Solid DOM/Yjs tests passed across 17 files; production build passed (99 modules; 203.63 kB JavaScript and 48.54 kB CSS before gzip).
- `git diff --check` passed.

### 2026-08-14 — WebSocket transport failures are contained and visible

- The browser transport now accepts downstream text only. `Blob`, `ArrayBuffer` and typed-array deliveries are rejected before JSON parsing; diagnostics include only category and byte/character count, never the frame body.
- A malformed delivery or one feature callback throwing cannot poison later WebSocket messages. The adapter contains the exception, reports a typed protocol issue and continues processing the next valid frame.
- Native `WebSocket.send` races now return `false` instead of escaping the command lifecycle. `CommandTracker` therefore never registers an action as sent when the browser rejected the write synchronously.
- Legacy bearer bootstrap no longer logs or surfaces the thrown send error, because a browser/polyfill could include the serialized auth frame in that message.
- Transport/protocol issues appear as deduplicated persistent cards in the existing Error Center. They remain dismissible and do not misuse short success-toast timing.

Verification:

- Focused transport and Error Center fault-injection tests passed 11/11, including binary payload redaction, malformed→valid continuation, consumer exceptions, synchronous send failure, failed-pong health semantics, status-callback isolation and legacy bearer redaction.
- `cd acp-hub/web && bun run test && bun run build` — 47/47 Node architecture/state contracts and 119/119 module/Solid DOM/Yjs tests passed across 18 files; the production bundle was served successfully.
- `git diff --check` passed; a loopback HTTP probe returned 200 with CSP, `nosniff`, frame denial and `Cache-Control: no-store` intact.

### 2026-08-14 — Simultaneous permission requests are a visible security queue

- The former `permissions()[0]` M3 placeholder is gone. Every server-projected pending request is discoverable through one focused queue with an explicit current/total count and Previous/Next controls.
- Queue navigation never submits a decision. The selected request follows `permission_id` through unrelated Yjs updates, and removal advances predictably at the same queue position rather than jumping back to an arbitrary map entry.
- Projection order is deterministic: valid expiry time first, then stable permission identity. The UI does not depend on Y.Map iteration order to decide which security question appears first.
- Decision locks remain scoped to their exact permission id, so one pending/uncertain choice cannot disable or mislabel another request. A malformed projection without an id is visible but fail-closed; Allow and Deny cannot emit an empty identifier.
- ARIA title/status ids are generated locally rather than derived from external protocol identifiers.

Verification:

- Focused Yjs/queue/card/MessageList tests passed 17/17, covering stable ordering, discovery, navigation without mutation, identity retention, removal, independent locks, malformed-id closure and production mounting.
- `cd acp-hub/web && bun run test && bun run build` — 48/48 Node architecture/state contracts and 126/126 module/Solid DOM/Yjs tests passed across 19 files; production build serves `index-9g1ZZOXr.js` and `index-DuRSCz2J.css`.
- `git diff --check` passed. The local service recovered its Registry and serves the same verified bundle from `127.0.0.1:8456`.

### 2026-08-14 — Conversation entries are a tested reading component

- `MessageList` no longer owns every message-role and evidence branch alongside scrolling and hydration. A dedicated `ConversationMessage` component owns the stable visual/semantic contract for user, system and assistant entries; the list is reduced from 231 to 107 lines and remains responsible only for collection behavior.
- The reader keeps the existing restrained hierarchy intentionally: user text is an inert, right-aligned surface; completed assistant content uses safe Markdown on the open reading plane; streaming assistant content remains plain text so partial Markdown cannot restructure the page.
- Reasoning is collapsed by default. Tool calls retain their existing tested disclosure. Resources are named, inert facts with full identifiers available, while projected errors are explicit named alerts rather than anonymous colored blocks.
- Empty assistant projections expose one screen-reader progress label. Streaming dots themselves are hidden and use a product-owned reduced-motion-aware keyframe instead of depending on a Tailwind implementation detail.
- Feature styles now use semantic `conversation-message`, `message-reasoning`, `message-resource` and `message-error` classes rather than embedding high-density utility strings into the domain component.

Verification:

- Focused `ConversationMessage` and production `MessageList` tests passed 8/8, covering inert user text, assistant Markdown/copy, streaming plain text, evidence layers, errors, empty progress and list mounting.
- The source architecture contract requires `MessageList → ConversationMessage` delegation and forbids the former inline `MessageBubble`/Markdown/tool composition.
- `cd acp-hub/web && bun run test && bun run build` — 49/49 Node architecture/state contracts and 131/131 module/Solid DOM/Yjs tests passed across 20 files.
- The production build passed at 101 modules: `index-BLNWKBRO.js` is 206.03 kB and `index-BbIiZ_Cn.css` is 47.90 kB before gzip, both smaller than the preceding verified bundle despite the stronger reader contract.
- `git diff --check` passed.
