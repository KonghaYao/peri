# ACP v2 stdio Preview

状态：现行（S1–S4 已落地：多会话连接、显式协商、错误码、幂等键、配置面、update 投影、
`session/list` 分页与 replay/deferred 语义）。默认关闭，仅 stdio 部署单元提供。

## Scope

Run `PERI_ACP_V2_PREVIEW=1 peri acp --cwd /absolute/project` with a configured provider.
An ACP client may request v2 in `initialize`. ACP v1 remains available on the
same entry point. The TUI, in-process transports, Swarm HTTP API, and the
unconfigured setup gate are unchanged.

The adapter shares the ACP Host and Agent mailbox. A prompt's success reply
comes from the existing committed-input delivery event, not its queue receipt.
The user-message ID is the native queue ID that became the transcript message
ID. A request withdrawn before commitment uses the same atomic store operation
as the normal take-back path. ACP v2 submissions do not interrupt an in-flight
model call the way explicit TUI send can.

Live message chunks and replay use existing message IDs. Cold replay streams
canonical retained history through the host's existing reader. The adapter adds
replacement boundaries for replayed chunks, the replay-completion marker, and
the current foreground-state snapshot. It does not invent a new history store.
A close that confirms some prompt withdrawals settles those prompts even when a
later withdrawal or the shared close fails.

## Connection and session state

- One connection holds one **slot** per session it attached. `session/new` and
  `session/resume` never reject because another session is attached; prompts,
  updates, cancellations and closes route by `sessionId`, and within one
  connection closing or deleting one session never affects the others. There is
  no cross-connection notification: deleting an idle session another connection has
  attached succeeds at the host and leaves that connection's slot `Attached`; it
  learns the outcome from the host on its next request, or by closing the slot.
  The shared host keeps its
  historical one-session-per-connection gate for ACP v1, the TUI and in-process
  transports; only a transport that declares
  `AcpTransport::allows_concurrent_sessions` (the v2 adapter) skips it.
- A v1 connection reproduces that gate at the adapter. A read-only
  `session/load` is **not** a setup: a page cursor or a `history` window is
  classified by the host before it reserves anything, answered from storage and
  never attached, so it passes without taking the gate and without binding the
  connection to the paged session. `session/new`, `session/fork`,
  `session/resume` and an initial `session/load` keep the setup admission. A
  request the classifier cannot treat as read-only history keeps that admission
  too: an unbound connection sees the host's own error shape, while a connection
  bound to another session (or with a setup in flight) is refused by the adapter
  first with `-32010`.
- Each slot owns a turn epoch and the foreground state machine
  `Idle → Running → RequiresAction → Running → Idle`. Turn start increments the
  epoch; the first outstanding permission/elicitation of a `Running` session
  publishes `requires_action`; the last one to complete in the same epoch
  publishes `running`; the turn end publishes `idle` with a stop reason. Only a
  foreground session warrants `requires_action` — an idle session's permission
  never changes the published state. Completions whose epoch no longer matches
  are dropped instead of publishing, so a cancelled or finished turn cannot
  resurrect `running`.
- A `session/resume` binds the slot in `Attaching` before the request is
  forwarded: session-metadata updates (`available_commands_update`,
  `config_option_update`, `current_mode_update`, `session_info_update`) the host
  pushes during setup are parked (bounded at 256). On a successful plain resume
  the state snapshot and then the parked metadata are written **before** the
  reply (v1 resume pushes the command table and options ahead of its own
  response); a new session's metadata follows its reply because the id is not
  routable before then. Live activity outside a replay is not parked, and a
  failed setup drops the slot it created.
- A stale success records that an attach completed but leaves the takeover's
  `Attaching` phase untouched; only the owning attempt publishes `Attached`.
  A failed attempt hands the slot back only to a still-pending attempt newer
  than the last completed attach; a pending attempt superseded by a committed
  attach is never resurrected, and its late failure can no longer delete the
  live slot. The handback restores that attempt's replay state (including the
  chunk identities already streamed) and keeps the slot `Attaching`, so live
  metadata cannot bypass the pending owner. Only when no pending owner remains
  and an attach had completed is `Attached` restored.
- Concurrent setups on one session are owned by the newest bound attempt. An
  older success still answers its client but neither ends that owner's replay
  (`replay_complete`) nor drains the metadata or deferred updates parked for it.
  If no attach ever completed and no setup is left in flight, the slot is
  dropped. A failed owner still ends its own replay state, so a stale success
  cannot leave live chunks carrying replay replacement boundaries.
- A failed replay that ends the hold with no successor publishes the state
  transition the hold swallowed (exactly the snapshot the successful path reports
  before its reply), including the stop reason (`cancelled`, `error`, …). A
  client that receives the `session_busy` refusal therefore still learns that the
  turn ended.
- A setup refused because the session's previous close has not finished is the
  exception to "a failed setup drops its slot": the host keeps that session
  reserved until an explicit close drains it, so the adapter keeps an
  `Attaching` slot for it. `session/close` therefore still reaches the host and
  can retry the cleanup, while the slot is not `Attached` and no prompt can
  address it. A normal setup failure still removes the slot it created.
- `session/cancel` is accepted while a turn is running through a pending
  cursorless resume (`Attaching`, no replay); only an idle slot whose only
  activity is an attaching setup drops it.
- `session/delete` is advertised and implemented as a durable delete: it always
  reaches the host, even for a session this connection never attached (the
  pinned `session/list` → `session/delete` → `session/list` flow), and answers
  the pinned empty payload. Only a successful reply settles this connection's
  parked prompts and drops its slot; the host's error for a session it cannot
  delete (for example `-32002`) is passed through and leaves the slot alone.

## Method surface

| method | behavior |
| --- | --- |
| `initialize` | `protocolVersion` ≥ 2 plus `info`; a present version that is not a JSON integer in `0..=65535` is `-32602` before any negotiation and leaves the connection unbound. Repeats are allowed only before the first other request. Declarations commit with a successful host reply. |
| `session/new` | Creates a session; always carries `configOptions` in the v2 shape. |
| `session/resume` | Attaches an existing session; `replayFrom` is optional. A cursorless resume of an already attached session only refreshes the state snapshot and metadata. |
| `session/list` | Keyset pagination (below). `auth/*`, `session/fork`, `providers/*`, `mcp/message`, unknown methods are never forwarded. |
| `session/close` | Closes this connection's slot; closing an unknown session is a success. |
| `session/delete` | Advertised via `session.delete`; always forwarded to the host (durable delete, slot or not), settles this connection's parked prompts and removes its slot only on success. |
| `session/prompt` | Admission, durable idempotency key, committed-delivery settlement, cancellation. `resource_link` content is passed as a text reference and is **deliberately never fetched**; `audio`/`resource` are `-32602`. |
| `session/set_config_option` | Validated against the option list last reported for the session (today `mode`, `model`, `thinking_effort`, built by `peri-acp/src/session/state_builders.rs`); the adapter defines no stable config-id list of its own. Unknown id/value is `-32602` and leaves the configuration untouched. |
| `session/cancel` | Turn-level cancel: withdraws queued prompts, stops the host turn, publishes `idle cancelled`. |
| `$/cancel_request` | Settles a `session/prompt` request the adapter can still withdraw with `-32800`; other request ids are ignored as pinned. |
| `session/request_permission` | Baseline peer request; drives `requires_action`. |
| `elicitation/create` | Only form elicitations; gated on the client's `elicitation.form` declaration. |
| `terminal/cancel`, `tool_call/cancel` | `-32602`: Peri never advertises per-object cancellation. |

## `session/list` pagination

- The cursor is opaque to the client and its only encoding is
  `base64url(JSON {"u": updatedAt, "i": threadId})`, exactly the store's
  `(updated_at, thread_id)` keyset position. A missing or `null` cursor asks for
  the first page. Malformed/undecodable cursors are `-32602` (Invalid params),
  never silently replaced by the first page.
- The default page size is 100 and is clamped to `1..=500`. The pinned
  `ListSessionsRequest` has no page-size field; the preview accepts a top-level
  `limit` as a documented extension so conformance probes can exercise every
  page boundary. Standard clients simply receive the default page.
- The requested `limit` is the **cap on the response**, not on one raw store
  page. `cwd` is an exact-directory filter (an absolute path; subdirectories and
  additional roots do not match). Storage does not accept a `cwd` predicate
  inside its keyset query, so matching is applied after each raw page and pages
  are fetched until the requested number of matches is collected or the store is
  exhausted. Each fetch asks only for the entries still missing from the
  response, so a response never contains more than `limit` sessions and a raw
  page is never partially consumed. `nextCursor` is the cursor of the **last raw
  page consumed** — not of the last match — so an entry filtered out of one
  response is returned by the next request instead of being skipped.
- The query travels as the preview-only internal request
  `_peri/stdio-v2/session-list`; the v1 `session/list` handler and its response
  shape are unchanged. The cursor stays valid across processes because it names
  a durable store position, not connection state. Preview-only internal methods
  (`_peri/stdio-v2/*`) are **not available on a v1 connection**: they answer
  `-32601` even when the preview transport is enabled.

## Replay semantics

- `replayFrom` supports exactly `{"type": "start"}`. Any other cursor type,
  including a custom `_peri/*` value (this preview defines none), is `-32602`.
  A cursorless resume never replays history.
- Replay and live traffic are distinguished by the host marker: the adapter
  requests `peri.replay` at `initialize`, so v1 replay notifications carry
  `_meta.periReplay = true` on the update. Replay-marked updates stream
  immediately (with one replacement boundary per message) and the adapter
  tracks chunk identities so a replayed message is replaced rather than appended
  twice. Everything else arriving while replay is streaming is live and is
  parked in a per-session queue; live chunks never reset a message.
- Parked live metadata is not merged blindly. `available_commands_update`,
  `config_option_update` and `current_mode_update` replace their whole table, so
  only the newest broadcast of each kind is kept (the host may repeat a table
  once per attach phase). `session_info_update` is a **patch** in the pinned v2
  schema — omitted fields keep their value — so every patch is kept and flushed
  in arrival order; merging them would drop the title or timestamp a later patch
  does not repeat.
- Order for one `session/resume` with a cursor: last replayed update →
  `replay_complete` (exactly once, including an empty replay) → current state
  snapshot (`idle`/`running`/`requires_action`) → response → parked live updates.
  A failed or truncated replay sends no marker and fails the request.
- A resume may arrive while the session's turn is still running. The adapter no
  longer pre-rejects it and can hold live updates, keep the foreground state
  machine current, and report the then-current state in a successful reply. The
  shared host, however, still refuses `session/load` (and a plain
  `session/resume`) with `-32010` while that session's per-session prompt lock is
  held by the executing turn; concurrent work against executing state has not
  been proven safe. The adapter therefore reports a stable `-32020` with
  `data.reason = "session_busy"` and `data.sessionId`, and the client should wait
  for `idle` (or the turn end) and retry. Mid-turn replay is **not** supported
  end to end today: what the adapter exercises in `host::stdio::v2::replay` is
  the hold/flush path and its 1024-update bound, which are ready for the day the
  host lifts the gate. That is a host policy decision (or a preview-only
  read-only replay route), tracked outside this stage. A `session/cancel`
  received during replay still withdraws and stops the turn; the turn's buffered
  live updates converge to `idle` in the normal way.
- `state_update` is a snapshot, not an event: transitions still run while the
  queue holds, but the post-replay snapshot is authoritative and individual
  parked state updates are not replayed. The adapter remembers the newest
  suppressed snapshot: a successful replay reports its stop reason through the
  authoritative snapshot, and a failed replay publishes it after the error
  reply, so a hold can never hide that the turn ended.
- The per-session live buffer is bounded at 1024 updates. Past the bound the
  adapter drops the newest update and logs a warning; the client can
  resynchronize with another `session/resume` (with or without a cursor). This
  is the only loss path for live updates during replay.
- A cursorless resume of a session already attached to this connection does not
  replay: it answers the state snapshot and the session metadata (configuration
  and command table) the host reports. A stale success from an older concurrent
  setup never emits the takeover's `replay_complete` and never flushes its
  buffer.

## Error mapping

The v2 adapter and the preview-only host handlers construct their errors from
the constructors and constants in `host/stdio/v2/errors.rs`, so request paths do
not carry bare code literals. That module is the review point, not a claim that
every error in the process is born there.

`-32000` is reserved for *Authentication required* by the pinned schema, so the
preview never emits it: host passthroughs that use it for backpressure are
rewritten to `-32020` with the original `data.reason` (or `server_error`).

`-32010` is the host's overloaded conflict code. The **only** v2 outbound path
that forwards it is the close-incomplete settlement of `session/close` (and the
shared close/delete gate behind it); any other close/delete failure is `-32603`. Every other host `-32010` is mapped in
`errors.rs::host_failure`: a transient setup refusal (`Session is executing`,
`Session is closing`) becomes `-32020` + `data.reason = "session_busy"` when the
session id is known; a setup refused because the session's close has not
finished (`data.reason = "session_cleanup_incomplete"`, or the restore
dispatch's `Session close incomplete: resume the close before loading this
session`) becomes `-32603` with `data.reason = "session_cleanup_incomplete"` and
`data.sessionId`; a setup whose client-supplied cwd/location cannot be used is
`-32602` (Invalid params, host message kept); list storage failures and prompt
workspace errors are `-32603` (Internal error, host message kept).

Every `-32002` the adapter constructs names the session it is about in
`data.sessionId`: the adapter's slot admission and the preview-only handler
behind `session/prompt`'s internal input endpoint. The request named that
session, so a client may drop its local state. Errors passed through from the
host keep the host's shape; in particular an MCP-over-ACP `serverId` conflict is
still a bare `-32002` from the v1 host path (pre-existing; not re-shaped here).
A `session/resume` configuration refresh that fails internally is not such a
response: the adapter only logs it and omits `configOptions` from the resume
reply.

`-32020` is **not** an execution-failure report: a failed execution ends the
turn with `state_update.error.code = -32603` and `stopReason = "error"`.
`-32020` is used only for capacity/backpressure and for a busy session.

| condition | code | data |
| --- | --- | --- |
| method not advertised or unknown (`auth/*`, `session/fork`, `providers/*`, `mcp/message`, unknown names) | `-32601` | — |
| invalid params: unsupported content block, empty prompt, key not 1..=255 characters, missing/relative `cwd`, unadvertised `additionalDirectories`/`systemPrompt`, unsupported replay/list cursor, malformed list cursor, non-integer list limit, repeated `initialize` below v2, v2 `initialize` without `info`, a present `protocolVersion` that is not a JSON integer in `0..=65535` | `-32602` | — |
| session-scoped request naming a session this connection has not attached, or a preview-only handler whose session is no longer active on the host | `-32002` | `{ "sessionId": … }` |
| `terminal/cancel`, `tool_call/cancel` (per-object cancel is never advertised) | `-32602` | — |
| prompt withdrawn before insertion, close/delete settlement, retry of a key whose submission was withdrawn in this session generation | `-32800` | — |
| the same key/identity with different content (in flight, or already inserted in the transcript) | `-32602` | — |
| capacity/backpressure | `-32020` | `{ "reason": "prompt_capacity" \| …, "sessionId": … }` |
| v2 setup refused because the session's turn is executing or the session is closing (host `-32010`) | `-32020` | `{ "reason": "session_busy", "sessionId": … }` |
| v2 setup refused because the session's previous close has not finished (host `-32010`, `reason = session_cleanup_incomplete` or the `Session close incomplete` prefix) | `-32603` | `{ "reason": "session_cleanup_incomplete", "sessionId": … }`; the slot stays `Attaching` so `session/close` can retry |
| v2 setup whose client-supplied cwd/location cannot be used (host `-32010`) | `-32602` | host message kept |
| `session/list` storage failure, prompt workspace/resource failure (host `-32010`) | `-32603` | host message kept |
| execution failure reported in the state snapshot | `-32603` | `state_update.error = { code: -32603, message: … }`, `stopReason = "error"` |
| close/delete gate timeout (`Session close incomplete`) | `-32010` | passthrough; every other close/delete failure is `-32603` with the host message |

## Capabilities and peer requests

- The agent advertises only `session.prompt.image`, `session.mcp.stdio`,
  `session.mcp.http` and `session.delete`. It does not advertise `authMethods`,
  `session.additionalDirectories`, `session.systemPrompt`, config controls or
  any `_meta` extension.
- `initialize` captures the client's `elicitation.form` / `elicitation.url`
  declarations and forwards them to the host; the adapter's own routing facts
  (`peri.userInputQueue`, `peri.replay`) stay under `clientCapabilities._meta`.
- Form elicitation is only sent when the client declared `elicitation.form`;
  otherwise the adapter answers the broker's `elicitation/create` with `-32601`
  so it takes the existing `InteractionFailure::MethodUnsupported` degradation.
  Permission requests are baseline and are never gated.

## Idempotency

- Message identity is the de-duplication mechanism, so a key survives a restart.
  A keyed submission's message id is `UUIDv5(namespace, "sessionId\u{1f}key")`
  with a frozen namespace constant; an unkeyed submission uses `UUIDv7`. That id
  is the `thread_mq.id` and the transcript `messages.message_id`, so the durable
  store's existing "same id already inserted" fact answers the retry — no
  receipt table, no schema change, no key column. Keys are scoped to one session
  and their recognition period is the message's retention period: deleting the
  message (session delete, rewind, compaction) forgets the key.
- A retry is answered from the store, not from connection state. The internal
  input endpoint replies `{ "messageId": id, "state": "queued" | "delivered" }`:
  `queued` waits for the committed-delivery event as before; `delivered` means
  the message is already in the transcript, so the adapter acknowledges the
  original id at once and does not echo a second `user_message` (the client is
  retrying a submission whose message is in its history), and the host skips the
  dispatch for it. A same-key request that is still in flight joins the first
  submission; different content under one key is `-32602` (in flight: the
  adapter's signature; after insertion: the stored payload compared with
  `mq_rows::same_payload` in both store backends).
- A key retired by a confirmed withdrawal cannot be re-inserted in the same
  session generation (the mailbox keeps the id terminal); such a retry is
  answered with the insertion-time cancellation error `-32800` instead of
  parking. After the process restarts the id is free again and the retry is a
  new submission, which matches "a retry with a forgotten key is a new
  submission".
- Prompt requests are settled by committed delivery, an already-inserted retry,
  a confirmed withdrawal, or close. If a queued turn fails before insertion, the
  host's done event names its run ticket rather than the affected input, so the
  adapter leaves pending prompt requests untouched instead of guessing which one
  failed.

## Deliberately not implemented

| surface | answer | protocol reason |
| --- | --- | --- |
| `auth/login`, `auth/logout` | `-32601`; no `authMethods` in `initialize` | Peri has no authentication model; the pinned schema says clients MUST NOT call these when `authMethods` is omitted/empty |
| `session/fork` | `-32601`; not advertised | The pinned v2 method set has no fork; the SDK marks it unstable and callable only when `session.fork` is advertised. ACP v1 keeps its existing fork |
| `providers/list`, `providers/set`, `providers/disable` | `-32601` | Not in the pinned method set; Peri has no runtime provider mutation surface (configuration file + `ConfigSource` only) and the SDK marks them unstable |
| `mcp/message` (both directions) | outbound `-32601`; inbound notification dropped | The pinned `session.mcp` transports are stdio/http only — there is no `McpServerAcp` and no ACP transport to declare |
| `terminal/cancel`, `tool_call/cancel` | `-32602` | Peri never advertises per-object cancel; the pinned schema forbids calling them unless the object advertised `capabilities.cancel` |
| `elicitation/complete` (agent → client notification) | never sent | Peri only issues form elicitations; it never sends a `url` elicitation, so there is no URL-mode completion to report |
| custom replay cursors (`_peri/*`) | `-32602` | This preview defines no implementation-specific replay position; the pinned rule requires rejection rather than substituting `start` |
| display terminals, optional config controls, other `_meta` extensions | unadvertised | Peri produces no agent-owned terminal and negotiates no extension beyond the two client routing facts above |

## Upstream comparison

[ACP v2 migration](https://agentclientprotocol.com/protocol/v2/migration).
The repository fixture `scripts/verification/fixtures/acp-v2-schema.json` pins
the upstream v2 schema (git blob `301e906d8566c71f378e563905c3437d9edfac4c`,
upstream schema commit `927e31867b31b1e3c961c7edee7e3680b90be718`); the
independent Rust SDK smoke uses `agent-client-protocol` 3.5.0's unmodified
`v2_one_shot_client` example. Evidence and known limits are in
[the verification record](../verification/acp-v2-stdio-preview.md).
