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
  updates, cancellations and closes route by `sessionId`, and closing or
  deleting one session never affects the others. The shared host keeps its
  historical one-session-per-connection gate for ACP v1, the TUI and in-process
  transports; only a transport that declares
  `AcpTransport::allows_concurrent_sessions` (the v2 adapter) skips it.
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
  `Attaching` phase untouched; only the owning attempt publishes `Attached`, and
  a failure restores it only when an attach had completed. Metadata arriving
  after a stale success therefore still hits the hold.
- Concurrent setups on one session are owned by the newest bound attempt. An
  older success still answers its client but neither ends that owner's replay
  (`replay_complete`) nor drains the metadata or deferred updates parked for it.
  A failed owner hands the slot to the newest attempt still in flight,
  restoring `Attached` only when an attach already completed; if no attach ever
  completed and none is left in flight, the slot is dropped. The failed owner
  still ends the replay state, so a stale success cannot leave live chunks
  carrying replay replacement boundaries.
- `session/delete` is advertised and implemented: it settles this connection's
  parked prompts, deletes the session, and drops the slot.

## Method surface

| method | behavior |
| --- | --- |
| `initialize` | `protocolVersion` ≥ 2 plus `info`; repeats are allowed only before the first other request. Declarations commit with a successful host reply. |
| `session/new` | Creates a session; always carries `configOptions` in the v2 shape. |
| `session/resume` | Attaches an existing session; `replayFrom` is optional. A cursorless resume of an already attached session only refreshes the state snapshot and metadata. |
| `session/list` | Keyset pagination (below). `auth/*`, `session/fork`, `providers/*`, `mcp/message`, unknown methods are never forwarded. |
| `session/close` | Closes this connection's slot; closing an unknown session is a success. |
| `session/delete` | Advertised via `session.delete`; settles and removes this connection's slot. |
| `session/prompt` | Admission, durable idempotency key, committed-delivery settlement, cancellation. |
| `session/set_config_option` | Validated against the option list last reported for the session; unknown id/value is `-32602` and leaves the configuration untouched. |
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
- `cwd` is an exact-directory filter (an absolute path; subdirectories and
  additional roots do not match). Storage does not accept a `cwd` predicate
  inside its keyset query, so matching is applied after each raw page and pages
  are fetched until the requested number of matches is collected or the store is
  exhausted. `nextCursor` is the cursor of the **last raw page consumed** — not
  of the last match — so an entry filtered out of one response is returned by
  the next request instead of being skipped.
- The query travels as the preview-only internal request
  `_peri/stdio-v2/session-list`; the v1 `session/list` handler and its response
  shape are unchanged. The cursor stays valid across processes because it names
  a durable store position, not connection state.

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
- Order for one `session/resume` with a cursor: last replayed update →
  `replay_complete` (exactly once, including an empty replay) → current state
  snapshot (`idle`/`running`/`requires_action`) → response → parked live updates.
  A failed or truncated replay sends no marker and fails the request.
- A resume may arrive while the session's turn is still running. The adapter no
  longer pre-rejects it: it parks live updates, keeps the foreground state
  machine current, and reports the then-current state in the reply. The shared
  host, however, still refuses `session/load` with `-32010` while that session's
  per-session prompt lock is held, so today the client observes `-32010` for a
  replay requested mid-turn and the adapter restores the slot and its state.
  Lifting that gate (or adding a preview-only read-only replay route) is a host
  policy decision tracked outside this stage; the adapter's hold/order behaviour
  is covered by `host::stdio::v2::replay` tests. A `session/cancel` received
  during replay still withdraws and stops the turn; the turn's buffered live
  updates converge to `idle` in the normal way.
- `state_update` is a snapshot, not an event: transitions still run while the
  queue holds, but the post-replay snapshot is authoritative and individual
  parked state updates are not replayed.
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

One module owns every v2 error. `-32000` is reserved for *Authentication
required* by the pinned schema, so the preview never emits it: host
passthroughs that use it for backpressure are rewritten to `-32020` with the
original `data.reason` (or `server_error`).

| condition | code | data |
| --- | --- | --- |
| method not advertised or unknown (`auth/*`, `session/fork`, `providers/*`, `mcp/message`, unknown names) | `-32601` | — |
| invalid params: unsupported content block, empty prompt, key not 1..=255 characters, missing/relative `cwd`, unadvertised `additionalDirectories`/`systemPrompt`, unsupported replay/list cursor, malformed list cursor, non-integer list limit, repeated `initialize` below v2 | `-32602` | — |
| session-scoped request naming a session this connection has not attached | `-32002` | `{ "sessionId": … }` |
| `terminal/cancel`, `tool_call/cancel` (per-object cancel is never advertised) | `-32602` | — |
| prompt withdrawn before insertion, close/delete settlement, retry of a key whose submission was withdrawn in this session generation | `-32800` | — |
| the same key/identity with different content (in flight, or already inserted in the transcript) | `-32602` | — |
| capacity/backpressure | `-32020` | `{ "reason": "prompt_capacity" \| …, "sessionId": … }` |
| host-passthrough close/cleanup failure | `-32010` | passthrough |

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
