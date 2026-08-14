# Prompt recovery provenance

Status: implemented additive contract (schema v5 + `session/prompt-status`).

## Problem

The browser can reconcile a live prompt with a persisted chat entry by exact
`source_command_id`, but browser memory is intentionally lost on refresh and a
server restart intentionally clears `project_sessions.last_chat_id`. Restoring
an old runtime identity would be incorrect: a logical project session may be
opened through a fresh chat and ACP process.

The server must eventually make unresolved prompt delivery facts discoverable
without persisting a second copy of prompt text or copying outbox state into
SQLite/Registry.

## Ownership

- SQLite owns logical session catalog identity and runtime activation
  provenance only.
- Per-chat outbox owns command delivery state.
- Persisted Chat/Session Docs own visible user-entry and terminal projection.
- Registry remains a derived navigation projection and must not become a second
  command-status writer.

## Implemented seam

Schema v5 adds an append-only logical-session activation history whose identity is
`(session_id, chat_id, activated_at, retired_at)`. A history row never means the
runtime is currently alive. The provenance row must be durable before that
runtime accepts a prompt command; an empty row after a crash is harmless.

The authenticated read-only `session/prompt-status` query is keyed only by logical `session_id`.
The server authorizes the session, resolves its historical chat ids internally,
enumerates safe outbox metadata, and joins exact `command_id`/`turn_id` evidence
from persisted docs. Clients must never use an arbitrary chat id to read history.

The response includes only command/turn id, normalized public status, safe
timestamps and a reviewed stable error code. It must not contain the
prompt body, credential material, raw error text, arbitrary ACP frames or an
assertion that a historical runtime has been restored.

## Conservative normalization

- `projected`: a persisted user entry has the exact `source_command_id`.
- `completed`: the exact turn has a persisted terminal projection and the
  outbox has crossed its durable terminal barrier.
- `failed`: the outbox contains terminal non-retryable failure evidence.
- `delivery_unknown`: provenance exists but facts are partial, contradictory,
  missing after retention, or remain in a non-terminal post-dispatch state.

Absence of evidence is never completed, failed, or proof that dispatch did not
happen. Conflicts degrade to `delivery_unknown` and diagnostics, never to a more
confident state. `evidenceIncomplete=true` explicitly reports a missing,
retained-away or corrupt historical store/projection; an empty result under
that flag is not an all-clear.

## Release evidence

1. Store enumeration remains internal and the wire response has no recovery
   payload or prompt-body field; the browser decoder rejects such fields.
2. Runtime provenance is written in the same SQLite transaction as session
   finalization, and live-runtime reuse records it before catalog commit.
3. v5 migration backfills the last pre-v5 runtime before restart recovery clears
   the process-local hint.
4. Exact user `source_command_id`, assistant terminal status and control-doc
   terminal status are joined with the per-chat outbox. Partial facts are never
   normalized upward to completed.
5. Authorization starts at the logical catalog session; the request contains no
   chat id. Full and read-only principals may query, while ordinary mutations
   remain forbidden to read-only principals.
6. Results are bounded to 200 records with unresolved states retained first;
   `truncated` and `evidenceIncomplete` preserve honesty under retention.
