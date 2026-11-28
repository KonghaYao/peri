# ACP v2 stdio Preview

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
cursor store. This preview retains Peri's one-active-session-per-connection
constraint: while a session is active, `session/new` and a resume of a
different session are rejected until the active session is closed, and a
resume of the active session itself is allowed. The preview does not support
starting a replay during an active turn, nor resynchronizing a session whose
turn is still running; resume is available once the turn ends. A close that
confirms some prompt withdrawals settles those prompts even when a later
withdrawal or the shared close fails.

## Negotiated surface

- Session new, list, resume with no cursor or `{ "type": "start" }`, close, prompt,
  and cancellation. Optional or unknown replay/list cursor types are rejected.
- Connection-local prompt keys. Repeat the same text/key to get the same
  acknowledgement without a second insertion. Keys are not persisted with the
  inserted message: a new connection or a process restart does not deduplicate
  a retry, which is outside this preview's scope. An unanswered prompt must not
  be automatically resent across restarts unless the caller has reconciled
  whether it was committed.
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
