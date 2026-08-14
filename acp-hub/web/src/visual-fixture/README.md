# Authenticated UI visual fixture

This development-only entry renders the real `AppShell` with deterministic,
synthetic store facts. It exists to inspect states that normally require a live
server, ACP process and browser cookie.

```bash
cd acp-hub/web
bun run visual:dev
# open http://127.0.0.1:5173/visual-fixture.html?scenario=conversation
```

Scenarios: `catalog`, `conversation`, `permission-streaming`,
`recovery-errors`, and `terminal-readonly`. Unknown values fall back to the
complete conversation.

## Security boundary

- The production `index.html`, `AuthGate`, transport and server routes never
  import or branch to this fixture.
- Normal `vite build` has one explicit input and does not emit this HTML,
  fixture JavaScript or fixture CSS.
- Scenario data is synthetic and contains no credential material.
- Fixture controls exercise rendering, focus, disclosures and local overlays.
  Server actions remain production-gated because the fixture never changes the
  store-private `ready` or `currentCid` transport state.

Run `bun run verify:production-boundary` after changing either entry. The
server Web route test independently proves the fixture is absent from embedded
assets.

`scripts/visual-contract.mjs` is the browser geometry/state assertion contract
used across the viewport matrix; screenshots are review evidence, not brittle
pixel baselines.
