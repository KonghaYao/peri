---
name: browser-automation
description: Drive the Peri browser with the peri-browser MCP tools. Use when inspecting, navigating or interacting with a web page through snapshots, uids and page tools.
---

# Browser automation with peri-browser

Use the peri-browser tool group from the loopback `peri-browser-gateway` HTTP
MCP server. Each initialized MCP session owns an endpoint slot and the pages it
creates. Separate sessions may use the same Kitesurf instance, but pages do not
isolate cookies, login state or origin storage; use separate engine profiles for
that. Sharing the same HTTP MCP session means sharing its pages and active tab.

## Connect before browsing

1. Call `browser_status` and use `nextSteps` to distinguish missing install,
   offline engine, and an already-bound endpoint that has become unreachable.
   The tool does not install, start, or bind anything.
2. If `kitesurf.installed` is false, explain what Kitesurf is and ask before
   using `kitesurf.installCommand`. Do not add `sudo` or install without consent.
3. Use the user's chosen running local instance, or arrange a start with
   `kitesurf serve --port <chosen-port>`. `recommendedEndpoint.url` comes from
   `--cdp-url`, `KITESURF_CDP_URL`, or the built-in recommendation, in that
   order. A recommendation is not an assignment or permission to use a port.
4. Call `browser_connect` with the explicit loopback HTTP/WS URL, including its
   port, even if the status probe says the suggested engine is listening.
5. Use `new_page`, `navigate_page`, `take_snapshot` and other page tools only
   after connecting. Use the token-free `monitorIndex` and `sessionId` to guide
   the user to the correct monitor; keep its token URL private.

Connecting to the same normalized endpoint again is idempotent
(`alreadyConnected: true` when reachable). A different reachable endpoint is
refused with `already_connected`. To switch, create a new MCP session and
explicitly connect there; do not restart the shared gateway. If the old endpoint
is bound but unreachable, check that instance instead of silently switching.

### Install, profile and port failures

- The installer fails: keep its original error output instead of paraphrasing it.
  Suggest `GITHUB_PROXY` / `GITHUB_API_PROXY` when the network needs a mirror, or
  the mise alternative `mise use -g github:KonghaYao/kitesurf@latest`; do not
  retry with `sudo`.
- `platform.supported` is false: the installer has no build for this platform.
  Report it and offer the mise alternative instead of forcing the download.
- `kitesurf serve` reports that the profile is in use: do not kill processes and
  do not delete profiles. Report it to the user and ask whether to reuse the
  running instance (pass its URL to `browser_connect`) or start another one.
- The chosen port is taken by something else: start kitesurf on another port
  after asking the user, and pass that URL to `browser_connect`. The recommended
  endpoint is only a suggestion.
- The engine answers but `browser_connect` still fails: report the exact
  `connect_failed` message; do not fall back to another URL silently.

## Pick the right tool

| Goal | Tool |
| --- | --- |
| Is the browser up, which pages exist, is the monitor available | `browser_status` |
| Attach this MCP session to a Kitesurf endpoint | `browser_connect` |
| Enumerate this session's pages | `list_pages` |
| Switch the active page | `select_page` |
| Open a page | `new_page` |
| Load a URL in the active page | `navigate_page` |
| Close a page and destroy its state | `close_page` |
| Read the page as text with element uids | `take_snapshot` |
| Look at the rendered page | `take_screenshot` |
| Read a runtime value | `evaluate_script` |
| Click, type, hover, press keys | `click`, `fill`, `hover`, `press_key` |
| Wait for content | `wait_for` |
| Hand the page to a human | `yield_page`, `request_human` |
| Show a user-facing authorization URL | `request_authorization`, then `AskUserQuestion` |

Prefer `take_snapshot` for automation: it is text and gives you uids. Use
`take_screenshot` when the question is visual (layout, overlap, colors).

## Standard loop

1. `navigate_page` (or `new_page`) with the target URL.
2. `wait_for` with the text or JS predicate that proves the page is ready.
3. `take_snapshot` to learn the structure and the element uids.
4. Interact with `click`, `fill`, `hover`, `press_key` using those uids.
5. After an action that changes the DOM, call `take_snapshot` again before the
   next uid-based action.

`navigate_page` waits for the URL to move and for `document.readyState` to become
`complete`, bounded to about 15 seconds. A slow page can still return with
`status: "loading"`; follow up with `wait_for` and a snapshot instead of
re-navigating.

## uids are short-lived

`take_snapshot` returns lines like:

```text
uid=1_4 button "Sign in"
```

The uid maps to one element in that snapshot only. Any navigation clears the
mapping. A `stale_uid` error means the page changed: take a fresh snapshot and
use a uid from the new result. Do not reuse uids across snapshots.

`click`, `fill` and `hover` also accept a CSS selector when the element has no
usable accessible node. Prefer a uid when the snapshot shows one.

## Filling inputs

`fill` focuses the element, selects the existing text, inserts the new text and
fires `input` and `change`. It replaces the field rather than appending. For a
search flow, `fill` then `press_key` with `Enter` is usually enough.

## Keys and waiting

- `press_key` takes chords such as `Enter`, `Tab`, `Escape`, `Control+A`,
  `Meta+ArrowLeft` or `Shift+Tab`.
- `wait_for` accepts `text` (substring of the visible text), `predicate` (a JS
  expression that must evaluate truthy), or only `timeout_ms`. It returns
  `matched: false` on timeout instead of failing, so the agent can decide what
  to do next.

## Page and ownership rules

- `list_pages` only lists targets this session created. `select_page` accepts a
  page id or one exact URL; an ambiguous URL is an error.
- `new_page` creates a blank target and attaches the controller before loading.
  `close_page` destroys the page state; it is not undoable.
- One writer per page. If a human takes over through the monitor, the controller
  moves to the monitor. The next page tool reclaims it automatically; a
  `page_busy_by_human` error means the monitor still holds it.
- `yield_page` deliberately gives the controller away while keeping the target
  and its JavaScript state. Use it only when a human needs to drive.
- `request_human` blocks the tool call until the monitor reports a result. Other
  page tools return `waiting_for_human` meanwhile.
- `request_authorization` validates the URL and returns it with an instruction;
  it does not navigate and does not need the browser. Call `AskUserQuestion`
  with that URL so the user can open it, and do not navigate to the URL before
  the user answers. A refusal is a decision; do not work around it.

## Failure handling

Errors are tool errors with a stable code and a hint. Common cases:

- `not_connected`: nothing is connected yet. Call `browser_status` and follow
  `nextSteps` (install, start, `browser_connect`), then retry. Do not report the
  error as a final answer.
- `connect_failed`: the URL did not answer `/json/version`. Start kitesurf (or
  correct the URL) and call `browser_connect` again; retrying page tools is
  useless until it succeeds.
- `already_connected`: this MCP process is attached to a different endpoint.
  Reuse the endpoint named in the error or restart the MCP process; do not
  retry with the second URL.
- `invalid_argument` on connect: pass `http://` or `ws://`, an explicit port and
  a loopback host (`127.0.0.1`, `::1` or `localhost`); TLS is not supported yet.
- `no_active_page`: call `new_page` or `list_pages` + `select_page`.
- `cdp_closed` / `target_expired`: the page is gone; list pages and recreate.
- `context_destroyed`: a navigation landed mid-command; retry after a snapshot.
- `provision_lock_timeout`: another session is creating or closing a page; retry.

Do not retry a write blindly after `target_expired`; the page state is gone.
