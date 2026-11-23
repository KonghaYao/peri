---
name: debugging
description: Debug a page through the peri-browser MCP. Use when a page misbehaves, a request fails, or console errors and rendered state need to be inspected.
---

# Debugging a page with peri-browser

Diagnostics come from the page controller: `Runtime` and `Network` are enabled
lazily the first time you ask for console or network data. Each page keeps the
newest 200 entries. Buffers start when the domain is enabled, so reload and
repeat the failing action if the event you need predates the first read.

## Order of investigation

1. `browser_status` — check the connection state first. If `connected` is false,
   follow `nextSteps` (ask before installing kitesurf, start
   `kitesurf serve --port 18971` in the background, wait for
   `recommendedEndpoint.listening`, then `browser_connect`); only then continue.
   A `not_connected` tool error means no page exists yet, not that the page is
   broken.
2. `take_screenshot` — see what the user sees. The result is a JPEG image
   (`image/jpeg`, quality 70, at most 1600x1000) delivered as MCP image content.
3. `list_console_messages` — errors and warnings, newest last as buffered.
4. `list_network_requests` — method, URL, status, MIME type and failure text.
   Pass `filter` to narrow by URL substring or HTTP method.
5. `get_network_request` with the `requestId` — request and response headers,
   status, timing and (when the response is still buffered by the browser) a
   bounded body up to 64 KiB.
6. `evaluate_script` — read runtime state that no tool exposes, for example
   `document.querySelectorAll('[role=dialog]').length` or a framework store.

`take_snapshot` complements this: it exposes the accessibility tree, not the
visual layout, and its uids are the way to interact with what you found.

## Console

`list_console_messages` returns entries with `level`, `text` and `timestampMs`.
Uncaught exceptions are recorded at `level: "error"`. Messages are formatted
from the CDP remote objects; objects are shown by description or JSON value, so
deeply nested objects are truncated by the browser, not by the MCP.

If the buffer is empty, the two usual causes are:

- `Runtime.enable` was never requested before the event; call the tool once,
  reproduce the action, then call it again.
- The page was navigated and the old document's events are gone; the buffer is
  per page, not per document, but enabling happens once per controller.

## Network

Requests are recorded from `Network.requestWillBeSent`, updated by
`responseReceived`, and marked finished by `loadingFinished` or
`loadingFailed`. Failed requests keep their `error` text (for example
`net::ERR_NAME_NOT_RESOLVED`). `get_network_request` returns `body` only when
the browser still has the response; a large, evicted or streaming body is
omitted rather than failing the tool.

When a request is missing entirely, confirm the action actually ran and check
`list_network_requests` without a filter. Redirects keep the same request id and
show the final response.

## Visual and layout failures

Use `take_screenshot` before and after the change. For geometry questions,
`evaluate_script` can return `getBoundingClientRect()` values, but prefer a
snapshot when the question is about reachable controls. For overflow, clipping
and z-index bugs, a screenshot is the primary evidence.

## Interacting while debugging

Click by uid from the latest `take_snapshot`. Use `wait_for` with a predicate
for app state (`window.__APP_READY__ === true`) or with text for rendered
content. If a click seems to do nothing, re-snapshot: the element may have been
replaced, and a stale uid is reported rather than silently clicking elsewhere.

## When a human is needed

- `request_human` asks the monitor's user to take over (for example to complete
  a captcha or a login the agent must not automate). The tool blocks; other page
  tools return `waiting_for_human` during the hand-off.
- `request_authorization` returns an authorization URL; call `AskUserQuestion`
  with it so the user can open it. It does not navigate the page, and you must
  not navigate to that URL before the user answers. A refusal is a decision; do
  not work around it by other means.
- `yield_page` releases the controller but keeps the target and JavaScript
  state, for a human using Chrome DevTools directly.

## Reporting findings

State which evidence you used: console entry text, request id and status,
screenshot, snapshot uid. Distinguish a failed request from a missing request,
and a stale uid from a missing element. If the diagnostics buffer cannot answer
the question, say which domain was not enabled or which event predates the
buffer rather than guessing.
