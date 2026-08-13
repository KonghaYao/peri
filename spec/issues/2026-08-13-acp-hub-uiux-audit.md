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

Verification:

- `cd acp-hub/web && bun run typecheck && bun run test && bun run build` — 32/32 Node contracts, 22/22 Solid DOM/Yjs tests, and production build passed (86 modules; 182.76 kB JavaScript before gzip).
- `bash -n acp-hub/dev.sh` and `git diff --check` — passed.
