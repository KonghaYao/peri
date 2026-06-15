import homepage from "./index.html";

const DEFAULT_SHELL = process.env.SHELL || "/bin/bash";
const PORT = parseInt(process.env.PORT || "3000");

Bun.serve({
    port: PORT,
    routes: {
        // ** HTML imports **
        // Bundle & route index.html to "/". This uses HTMLRewriter to scan
        // the HTML for `<script>` and `<link>` tags, runs Bun's JavaScript
        // & CSS bundler on them, transpiles any TypeScript, JSX, and TSX,
        // downlevels CSS with Bun's CSS parser and serves the result.
        "/": homepage,
    },
    fetch(req, server) {
        const url = new URL(req.url);

        if (url.pathname === "/ws") {
            if (
                server.upgrade(req, {
                    data: {
                        shell: url.searchParams.get("shell") || DEFAULT_SHELL,
                        args: url.searchParams.get("args") || "",
                        cols: url.searchParams.get("cols") || "80",
                        rows: url.searchParams.get("rows") || "24",
                    },
                })
            )
                return;
            return new Response("WebSocket upgrade failed", { status: 500 });
        }

        return new Response("Not Found", { status: 404 });
    },

    websocket: {
        open(ws) {
            const { shell, args: argsStr, cols, rows } = ws.data;
            const args = argsStr ? argsStr.split(" ").filter(Boolean) : [];

            const proc = Bun.spawn([shell, ...args], {
                env: { ...process.env, TERM: "xterm-256color" } as Record<
                    string,
                    string
                >,
                cwd: process.env.HOME,
                terminal: {
                    cols: parseInt(cols),
                    rows: parseInt(rows),
                    data(_terminal, data) {
                        const str =
                            typeof data === "string"
                                ? data
                                : new TextDecoder().decode(data);
                        try {
                            ws.send(str);
                        } catch {}
                    },
                    exit(_terminal, exitCode) {
                        try {
                            ws.send(
                                `\r\n[process exited with code ${exitCode}]\r\n`,
                            );
                            ws.close();
                        } catch {}
                    },
                },
            });

            proc.terminal.ref();
            (ws as any)._proc = proc;
        },

        message(ws, message) {
            const proc = (ws as any)._proc;
            if (!proc) return;

            const str =
                typeof message === "string"
                    ? message
                    : new TextDecoder().decode(message as ArrayBuffer);

            try {
                const msg = JSON.parse(str);
                if (msg.type === "resize") {
                    proc.terminal.resize(msg.cols, msg.rows);
                    return;
                }
            } catch {}

            proc.terminal.write(str);
        },

        close(ws) {
            const proc = (ws as any)._proc;
            if (proc) {
                try {
                    proc.kill();
                } catch {}
                proc.terminal.close();
            }
        },
    },
});

console.log(`PTY server listening on http://localhost:${PORT}`);
