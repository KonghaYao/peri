#!/usr/bin/env node
// Local browser-callback fixture for TUI URL elicitation acceptance.
// Usage: node browser_auth_server.mjs <port> <auth-file>
// GET /authorize renders a page that auto-redirects to /callback.
// GET /callback writes the auth file (the external service's success fact).

import fs from "node:fs";
import http from "node:http";

const port = Number(process.argv[2] ?? "0");
const authFile = process.argv[3] ?? "/tmp/peri-authorized.flag";

const server = http.createServer((req, res) => {
  if (req.url?.startsWith("/authorize")) {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(`<!doctype html><meta charset="utf-8">
<title>Peri authorization</title>
<h1>External authorization</h1>
<p>Redirecting to the local callback…</p>
<script>setTimeout(() => { location.href = "/callback"; }, 150);</script>`);
    return;
  }
  if (req.url?.startsWith("/callback")) {
    fs.writeFileSync(authFile, "authorized\n");
    console.log(`callback ${new Date().toISOString()}`);
    res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
    res.end("authorized\n");
    return;
  }
  res.writeHead(404);
  res.end("not found\n");
});

server.listen(port, "127.0.0.1", () => {
  const address = server.address();
  console.log(`AUTH_PORT=${address.port}`);
});
