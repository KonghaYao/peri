# ACP v2 stdio Preview

状态：现行（S1 已落地多会话连接、显式协商与错误码映射；幂等持久化、配置面与
replay 语义仍是后续阶段）

## Scope

Run `PERI_ACP_V2_PREVIEW=1 peri acp --cwd /absolute/project` with a configured provider.
An ACP client may request v2 in `initialize`. ACP v1 remains available on the
same entry point. The TUI, in-process transports, Swarm HTTP API, and the
unconfigured setup gate are unchanged.

The adapter shares the ACP Host and Agent mailbox. A prompt's success reply
comes from the existing committed-input delivery event, not its queue receipt.
The user-message ID is the native queue ID that became the transcript message
ID. A request withdrawn before commitment uses the same atomic store operation
as the normal take-back path; its admission to the wire is not a second source
of truth. ACP v2 submissions do not interrupt an in-flight model call the way
explicit TUI send can.

Live message chunks and replay use existing message IDs. Cold replay streams
canonical retained history through the host's existing reader. The v2 adapter
adds replacement boundaries for replayed chunks, the replay-completion marker,
and the current foreground-state snapshot. It does not invent a new history or
cursor store. A close that confirms some prompt withdrawals settles those
prompts even when a later withdrawal or the shared close fails.

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
- `session/resume` binds the slot in `Attaching` before the request is
  forwarded: session-metadata updates (`available_commands_update`,
  `config_option_update`, `current_mode_update`, `session_info_update`) the host
  pushes during setup are parked (bounded) and flushed right after the resume
  reply instead of being dropped. Live activity is not parked, and a failed
  setup drops the slot it created. `session/new` cannot park anything: the
  client does not know the new id until the reply.
- Replay still streams before the reply and a session whose turn is running
  cannot start one; resynchronizing a session with a live turn stays out of
  scope until replay work lands.
- `session/delete` is advertised and implemented: it settles this connection's
  parked prompts, deletes the session, and drops the slot.

## Error mapping

One module owns every v2 error. `-32000` is reserved for *Authentication
required* by the pinned schema, so the preview never emits it: host
passthroughs that use it for backpressure are rewritten to `-32020` with the
original `data.reason` (or `server_error`).

| condition | code | data |
| --- | --- | --- |
| method not advertised or unknown (`auth/*`, `session/fork`, `providers/*`, `mcp/message`, `session/set_config_option`, …) | `-32601` | — |
| invalid params: unsupported content block, empty prompt, key not 1..=255 characters, missing/relative `cwd`, unadvertised `additionalDirectories`/`systemPrompt`, unsupported replay/list cursor, repeated `initialize` below v2 | `-32602` | — |
| session-scoped request naming a session this connection has not attached | `-32002` | `{ "sessionId": … }` |
| `terminal/cancel`, `tool_call/cancel` (per-object cancel is never advertised) | `-32602` | — |
| prompt withdrawn before insertion, close/delete settlement | `-32800` | — |
| capacity/backpressure, replay against a running turn | `-32020` | `{ "reason": "prompt_capacity" \| "session_busy" \| …, "sessionId": … }` |
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
  so it takes the existing `InteractionFailure::MethodUnsupported` degradation
  instead of waiting on a request the client will reject. Permission requests
  are baseline and are never gated.
- No authentication is introduced: `initialize` carries no `authMethods` and
  `auth/*` stays `-32601`.

## Cancellation

- `session/cancel` bumps that session's epoch, withdraws its queued prompts,
  and only then asks the host to stop the turn; the resulting done event
  publishes `idle` with `cancelled`.
- `$/cancel_request` acts only on a `session/prompt` request the adapter can
  still withdraw — its caller gets `-32800`. Other in-flight request ids are
  ignored as the pinned spec allows, and each of those requests still receives
  its own reply, so nothing is left dangling.
- Per-object cancellation is never advertised: `terminal/cancel` and
  `tool_call/cancel` are rejected with `-32602`.

## Negotiated surface

- Session new, list, resume with no cursor or `{ "type": "start" }`, close,
  delete, prompt, and cancellation. Optional or unknown replay/list cursor
  types are rejected.
- Connection-local prompt keys. Repeat the same text/key to get the same
  acknowledgement without a second insertion. Keys are not persisted with the
  inserted message: a new connection or a process restart does not deduplicate
  a retry, which is outside this preview's scope. An unanswered prompt must not
  be automatically resent across restarts unless the caller has reconciled
  whether it was committed. Receipts and pending waiters are per session, so the
  capacity bound applies to the session that hit it.
- A prompt request is settled by committed delivery, a confirmed withdrawal, or
  close. If a queued turn fails before insertion, the host's done event names
  its run ticket rather than the affected input, so the adapter leaves pending
  prompt requests untouched instead of guessing which one failed.
- Tool-call updates, permission requests, elicitation, text and image prompts.
  The negotiated v2 connection uses Peri’s default approval policy; v1 keeps
  its existing stdio policy.
  Resource links are passed as text references, not fetched. Historical
  attachments retain Peri's existing bounded replay policy.
- Preview intentionally leaves out optional config controls, authentication RPC,
  display terminals, and other non-advertised extensions. ACP v1 clients still
  have their existing capabilities and private Peri methods.

Upstream comparison: [ACP v2 migration](https://agentclientprotocol.com/protocol/v2/migration).
The fixture pins upstream schema commit
`927e31867b31b1e3c961c7edee7e3680b90be718`; the independent Rust SDK smoke uses
`agent-client-protocol` 3.5.0's unmodified `v2_one_shot_client` example.
